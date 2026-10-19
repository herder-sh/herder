//! The client profile: every paired machine, in `<config_dir>/machines.json`.
//!
//! The file holds each machine's device key, so it is private to the user (mode 0600 in a 0700
//! directory) and replaced atomically on every change.
//!
//! Several clients may share a config dir at once, as the Mac app and the TUI do. Each change
//! is a read-modify-write of the file under an exclusive lock on `machines.lock` ([`update`]),
//! so no client overwrites another's change with its own stale copy, and every client re-reads
//! the file when it changes ([`stamp`]).

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use herder_protocol::HostId;
use serde::{Deserialize, Serialize};

use crate::Error;

const FILE: &str = "machines.json";
const LOCK: &str = "machines.lock";

/// A paired machine as saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedMachine {
    /// The daemon's host id, from its hello.
    pub(crate) host_id: HostId,
    /// The daemon's host name, from its hello.
    pub(crate) name: String,
    /// Addresses from the pairing link, as `host:port`, tried in order.
    pub(crate) addresses: Vec<String>,
    /// SHA-256 of the daemon's certificate, lowercase hex.
    pub(crate) fingerprint: String,
    /// This device's key for the machine, as PEM; secret.
    pub(crate) device_key: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Profile {
    machines: Vec<SavedMachine>,
}

/// The machines saved in `dir`; none when the file does not exist yet.
pub(crate) fn load(dir: &Path) -> Result<Vec<SavedMachine>, Error> {
    let path = dir.join(FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Profile>(&bytes)
            .map(|profile| profile.machines)
            .map_err(|err| Error::Local {
                message: format!("{} is not a valid profile: {err}", path.display()),
            }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(Error::Local {
            message: format!("reading {}: {err}", path.display()),
        }),
    }
}

/// Applies `change` to the machines saved in `dir`, as the file holds them now, and saves the
/// result, all under an exclusive lock that every client sharing `dir` takes: the machines as
/// saved, and what `change` returned. Nothing is saved when `change` fails.
pub(crate) fn update<T>(
    dir: &Path,
    change: impl FnOnce(&mut Vec<SavedMachine>) -> Result<T, Error>,
) -> Result<(Vec<SavedMachine>, T), Error> {
    let lock = lock(dir).map_err(|message| Error::Local {
        message: format!("locking the profile: {message}"),
    })?;
    let mut machines = load(dir)?;
    let changed = change(&mut machines)?;
    save(dir, &machines)?;
    drop(lock);
    Ok((machines, changed))
}

/// Takes the exclusive lock on the profile in `dir`, creating `dir` if missing; held until the
/// file is dropped.
fn lock(dir: &Path) -> Result<File, String> {
    create_private_dir(dir)?;
    let path = dir.join(LOCK);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|err| format!("opening {}: {err}", path.display()))?;
    file.lock()
        .map_err(|err| format!("locking {}: {err}", path.display()))?;
    Ok(file)
}

/// What identifies the profile file's current version: it is replaced, never written in
/// place, so a change gives it a new inode. `None` while there is no file.
pub(crate) fn stamp(dir: &Path) -> Option<(u64, u64, i64, i64, u64)> {
    let meta = fs::metadata(dir.join(FILE)).ok()?;
    Some((
        meta.dev(),
        meta.ino(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.size(),
    ))
}

/// Replaces the saved machines in `dir` with `machines`.
pub(crate) fn save(dir: &Path, machines: &[SavedMachine]) -> Result<(), Error> {
    let json = serde_json::to_vec_pretty(&Profile {
        machines: machines.to_vec(),
    })
    .map_err(|err| Error::Local {
        message: format!("encoding the profile: {err}"),
    })?;
    write_private(dir, FILE, &json).map_err(|message| Error::Local {
        message: format!("saving the profile: {message}"),
    })
}

/// Replaces `dir/file` with `bytes` atomically, private to the user: mode 0600, in `dir`
/// created with mode 0700 if missing.
/// Each write's temporary file is its own, so clients sharing a config dir can replace the
/// same file at once.
pub(crate) fn write_private(dir: &Path, file: &str, bytes: &[u8]) -> Result<(), String> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    create_private_dir(dir)?;
    let tmp = dir.join(format!(
        "{file}.{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let write = || -> io::Result<()> {
        let mut out = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        fs::rename(&tmp, dir.join(file))?;
        File::open(dir)?.sync_all()
    };
    write().map_err(|err| {
        let _ = fs::remove_file(&tmp);
        format!("writing {}: {err}", dir.join(file).display())
    })
}

/// Creates `dir` with mode 0700 if missing.
fn create_private_dir(dir: &Path) -> Result<(), String> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| format!("creating {}: {err}", dir.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn machine(host_id: &str) -> SavedMachine {
        SavedMachine {
            host_id: HostId::new(host_id),
            name: "box".into(),
            addresses: vec!["127.0.0.1:7447".into()],
            fingerprint: "ab".repeat(32),
            device_key: "secret".into(),
        }
    }

    #[test]
    fn machines_survive_a_save_and_the_file_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("herder");
        assert_eq!(load(&dir).unwrap(), Vec::new());
        assert_eq!(stamp(&dir), None);
        let machine = machine("host-1");
        save(&dir, std::slice::from_ref(&machine)).unwrap();
        let first = stamp(&dir);
        assert!(first.is_some());
        save(&dir, std::slice::from_ref(&machine)).unwrap();
        assert_ne!(stamp(&dir), first);
        assert_eq!(load(&dir).unwrap(), [machine]);
        let mode = fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::write(dir.join(FILE), "not json").unwrap();
        assert!(matches!(load(&dir), Err(Error::Local { .. })));
    }

    /// Updates from many writers at once, as from clients sharing the dir, all land.
    #[test]
    fn concurrent_updates_lose_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("herder");
        std::thread::scope(|scope| {
            for writer in 0..4 {
                let dir = &dir;
                scope.spawn(move || {
                    for n in 0..25 {
                        update(dir, |machines| {
                            machines.push(machine(&format!("host-{writer}-{n}")));
                            Ok(())
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(load(&dir).unwrap().len(), 100);
        // A failed change saves nothing.
        let failed = update(&dir, |machines| {
            machines.clear();
            Err::<(), _>(Error::Closed)
        });
        assert_eq!(failed, Err(Error::Closed));
        assert_eq!(load(&dir).unwrap().len(), 100);
    }

    /// Writers replacing the same file at once each use their own temporary file.
    #[test]
    fn concurrent_writes_of_one_file_all_succeed() {
        let tmp = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..50 {
                        write_private(tmp.path(), "file.json", b"{}").unwrap();
                    }
                });
            }
        });
        let files: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(files.len(), 1);
    }
}
