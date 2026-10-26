use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use herder_protocol::{Role, ServerMessage, SessionId};
use sha2::{Digest, Sha256};

use super::*;
use crate::hub::Outbox;
use crate::resources::{Budget, ReadHost, Reading, Ticket};

const LATEST: &str = "99.0.0";
const NEW_BINARY: &[u8] = b"herder 99.0.0";

/// Serves a release dir laid out like GitHub Releases, `/latest` redirecting to `LATEST`'s
/// tag, with `checksum` as the tarball's `.sha256`, or its real one.
fn release_server(checksum: Option<&str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let target = release::this_target().unwrap();
    let name = format!("herder-{LATEST}-{target}");
    let tarball = release::tarball(&[(&format!("{name}/herder"), NEW_BINARY)]);
    let sum = checksum.map_or_else(
        || {
            format!(
                "{}  {name}.tar.gz\n",
                release::hex(&Sha256::digest(&tarball))
            )
        },
        str::to_owned,
    );
    let download = format!("/download/v{LATEST}/{name}.tar.gz");
    let files = HashMap::from([
        (format!("{download}.sha256"), sum.into_bytes()),
        (download, tarball),
    ]);
    let tag = format!("{base}/tag/v{LATEST}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let path = request.split(' ').nth(1).unwrap_or("/");
            let mut response = match files.get(path) {
                _ if path == "/latest" => format!(
                    "HTTP/1.1 302 Found\r\nLocation: {tag}\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                )
                .into_bytes(),
                Some(body) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes(),
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            if let Some(body) = files.get(path) {
                response.extend(body);
            }
            stream.write_all(&response).unwrap();
        }
    });
    base
}

/// A host admitting turns by the turn limit only.
struct Unread;

impl ReadHost for Unread {
    fn read(&self) -> anyhow::Result<Reading> {
        anyhow::bail!("no readings in tests")
    }
}

struct Daemon {
    _dir: tempfile::TempDir,
    exe: std::path::PathBuf,
    hub: Arc<Hub>,
    owner: Arc<Outbox>,
    admission: Arc<Admission>,
    shutdown: CancellationToken,
    updater: Arc<Updater>,
}

impl Daemon {
    fn new(base: String) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("herder");
        std::fs::write(&exe, b"herder 0.1.0").unwrap();
        let hub = Arc::new(Hub::default());
        let owner = Arc::new(Outbox::default());
        hub.connect(&owner, Role::Owner);
        let budget = Budget {
            cores: 4,
            max_turns: 4,
            min_memory_available: 0,
            max_memory_pressure: 100.0,
            max_load: 100.0,
        };
        let admission = Arc::new(Admission::new(budget, Box::new(Unread)));
        let shutdown = CancellationToken::new();
        let update = SelfUpdate {
            exe: exe.clone(),
            base,
        };
        let updater = Arc::new(Updater::new(
            Some(update),
            Some(Arc::clone(&admission)),
            shutdown.clone(),
        ));
        Self {
            _dir: dir,
            exe,
            hub,
            owner,
            admission,
            shutdown,
            updater,
        }
    }

    fn start(&self, when: UpdateWhen) -> Result<CommandResult, ErrorInfo> {
        self.updater.start(when, &self.hub)
    }

    /// The next stage clients are sent.
    async fn next(&self) -> UpdateStage {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                while let Some(message) = self.owner.pop() {
                    if let ServerMessage::DaemonUpdate { stage } = message {
                        return stage;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the update moves on")
    }

    /// The next stages clients are sent, `count` of them.
    async fn stages(&self, count: usize) -> Vec<UpdateStage> {
        let mut stages = Vec::new();
        for _ in 0..count {
            stages.push(self.next().await);
        }
        stages
    }

    fn binary(&self) -> Vec<u8> {
        std::fs::read(&self.exe).unwrap()
    }
}

fn turn() -> SessionId {
    SessionId::new(ulid::Ulid::generate().to_string())
}

fn versions() -> (String, String) {
    (env!("CARGO_PKG_VERSION").to_owned(), LATEST.to_owned())
}

#[tokio::test]
async fn the_latest_release_replaces_the_binary_and_the_daemon_restarts() {
    let daemon = Daemon::new(release_server(None));
    assert_eq!(daemon.start(UpdateWhen::Now), Ok(CommandResult::Applied));
    let (from, to) = versions();
    assert_eq!(
        daemon.stages(2).await,
        [
            UpdateStage::Downloading {
                from: from.clone(),
                to: to.clone()
            },
            UpdateStage::Restarting { from, to },
        ]
    );
    assert_eq!(daemon.binary(), NEW_BINARY);
    tokio::time::timeout(Duration::from_secs(5), daemon.shutdown.cancelled())
        .await
        .expect("the daemon stops to restart");
    assert!(updated());
    assert!(
        matches!(daemon.admission.request(&turn()), Ticket::Waiting(_)),
        "no turn starts that the restart would end"
    );
}

#[tokio::test]
async fn a_checksum_mismatch_changes_nothing() {
    let wrong = format!("{}  herder.tar.gz\n", "0".repeat(64));
    let daemon = Daemon::new(release_server(Some(&wrong)));
    daemon.start(UpdateWhen::Now).unwrap();
    let stages = daemon.stages(2).await;
    let Some(UpdateStage::Failed { reason }) = stages.last() else {
        panic!("expected a failure: {stages:?}");
    };
    assert!(reason.contains("checksum mismatch"), "{reason}");
    assert_eq!(daemon.binary(), b"herder 0.1.0");
    assert!(!daemon.shutdown.is_cancelled());
    assert!(
        matches!(daemon.admission.request(&turn()), Ticket::Admitted(_)),
        "turns start again"
    );
    daemon
        .start(UpdateWhen::Now)
        .expect("a failed update can be tried again");
}

#[tokio::test]
async fn idle_waits_for_the_running_turn_and_holds_new_ones() {
    let daemon = Daemon::new(release_server(None));
    let Ticket::Admitted(running) = daemon.admission.request(&turn()) else {
        panic!("the turn runs");
    };
    daemon.start(UpdateWhen::Idle).unwrap();
    assert_eq!(
        daemon.next().await,
        UpdateStage::WaitingForIdle { running_turns: 1 }
    );
    let Ticket::Waiting(mut held) = daemon.admission.request(&turn()) else {
        panic!("no new turn starts while the update waits");
    };
    assert_eq!(
        daemon.start(UpdateWhen::Idle).unwrap_err().code,
        ErrorCode::Conflict
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(daemon.binary(), b"herder 0.1.0", "nothing changes yet");

    drop(running);
    let (from, to) = versions();
    assert_eq!(
        daemon.stages(2).await,
        [
            UpdateStage::Downloading {
                from: from.clone(),
                to: to.clone()
            },
            UpdateStage::Restarting { from, to },
        ]
    );
    assert_eq!(daemon.binary(), NEW_BINARY);
    assert!(held.try_recv().is_err(), "the held turn never starts here");
}

#[tokio::test]
async fn now_starts_an_update_waiting_for_turns() {
    let daemon = Daemon::new(release_server(None));
    let Ticket::Admitted(_running) = daemon.admission.request(&turn()) else {
        panic!("the turn runs");
    };
    daemon.start(UpdateWhen::Idle).unwrap();
    assert_eq!(
        daemon.next().await,
        UpdateStage::WaitingForIdle { running_turns: 1 }
    );
    assert_eq!(daemon.start(UpdateWhen::Now), Ok(CommandResult::Applied));
    let stages = daemon.stages(2).await;
    assert!(
        matches!(stages.last(), Some(UpdateStage::Restarting { .. })),
        "{stages:?}"
    );
    assert_eq!(daemon.binary(), NEW_BINARY);
}

#[tokio::test]
async fn refused_where_the_binary_cannot_be_replaced_or_restarted() {
    let daemon = Daemon::new(release_server(None));
    let unmanaged = Arc::new(Updater::new(None, None, CancellationToken::new()));
    let refused = unmanaged.start(UpdateWhen::Now, &daemon.hub).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);

    let dir = daemon.exe.parent().unwrap();
    let mode = |mode| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    mode(0o555);
    // Root writes to a read-only dir all the same, so there is nothing to refuse.
    let root = std::fs::write(dir.join("probe"), b"").is_ok();
    let refused = daemon.start(UpdateWhen::Now);
    mode(0o755);
    if !root {
        let refused = refused.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Unsupported);
        assert!(
            refused.message.contains("cannot write"),
            "{}",
            refused.message
        );
        assert_eq!(daemon.binary(), b"herder 0.1.0");
    }
}
