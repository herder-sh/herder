//! The client profile: every paired machine, in `<config_dir>/machines.json`.
//!
//! The file holds each machine's device key, so it is private to the user (mode 0600 in a 0700
//! directory) and replaced atomically on every change. One [`crate::Client`] owns a config dir
//! at a time.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use herder_protocol::HostId;
use serde::{Deserialize, Serialize};

use crate::Error;

const FILE: &str = "machines.json";

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

/// Replaces the saved machines in `dir` with `machines`.
pub(crate) fn save(dir: &Path, machines: &[SavedMachine]) -> Result<(), Error> {
    let failed = |what: &str, err: io::Error| Error::Local {
        message: format!("{what} {}: {err}", dir.display()),
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| failed("creating", err))?;
    let json = serde_json::to_vec_pretty(&Profile {
        machines: machines.to_vec(),
    })
    .map_err(|err| Error::Local {
        message: format!("encoding the profile: {err}"),
    })?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    let write = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join(FILE))?;
        File::open(dir)?.sync_all()
    };
    write().map_err(|err| failed("saving the profile in", err))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn machines_survive_a_save_and_the_file_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("herder");
        assert_eq!(load(&dir).unwrap(), Vec::new());
        let machine = SavedMachine {
            host_id: HostId::new("host-1"),
            name: "box".into(),
            addresses: vec!["127.0.0.1:7447".into()],
            fingerprint: "ab".repeat(32),
            device_key: "secret".into(),
        };
        save(&dir, std::slice::from_ref(&machine)).unwrap();
        assert_eq!(load(&dir).unwrap(), [machine]);
        let mode = fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::write(dir.join(FILE), "not json").unwrap();
        assert!(matches!(load(&dir), Err(Error::Local { .. })));
    }
}
