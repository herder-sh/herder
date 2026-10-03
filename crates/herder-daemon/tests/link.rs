//! Linking a running host to a vault from a client, as the TUI's machines view does: a client
//! that owns both asks the vault for a host-only code and hands it to the host with the
//! vault's address and fingerprint; the host keeps `[vault]` in its config and replicates
//! without a restart. Stopping removes `[vault]` and stops replicating, and the client revokes
//! the host's device on the vault. Members can do none of it, and a host code pairs no client.
//!
//! The vault and the host are `herder_daemon::vault::serve` and `herder_daemon::serve`, the
//! functions `herder daemon` runs, in this process, reached through their control sockets and
//! their TLS WebSocket ports.

use std::path::{Path, PathBuf};
use std::time::Duration;

use herder_client_core::{Client, Error, Machine, PairingUri};
use herder_daemon::auth::DeviceRole;
use herder_daemon::auth::control::{self, Request, Response};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, ErrorCode, EventBody, HostId, LinkedVault,
    PermissionMode, Provider, Role, SessionId, Timestamp,
};
use herder_store::{NewEvent, Store};
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(30);

/// A daemon's config file, data dir, and the task serving it.
struct Daemon {
    config: PathBuf,
    data_dir: PathBuf,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    /// Starts a daemon on `dir` whose config file starts with `extra`; a vault if `vault`.
    async fn start(dir: &Path, extra: &str, vault: bool) -> Self {
        let data_dir = dir.join("data");
        let config = dir.join("daemon.toml");
        let mode = if vault { "mode = \"vault\"\n" } else { "" };
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            &config,
            format!(
                "{extra}listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n{mode}",
                data_dir.to_str().unwrap()
            ),
        )
        .unwrap();
        let loaded = herder_daemon::Config::load(Some(&config)).unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                if vault {
                    herder_daemon::vault::serve(&loaded, shutdown).await
                } else {
                    let accounts = Default::default();
                    let adapters = Default::default();
                    herder_daemon::serve(&loaded, adapters, Default::default(), accounts, shutdown)
                        .await
                }
            }
        });
        let socket = data_dir.join(control::SOCKET);
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !socket.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no control socket");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            config,
            data_dir,
            shutdown,
            task,
        }
    }

    async fn control(&self, request: Request) -> Response {
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || control::request(&data_dir, &request).unwrap())
            .await
            .unwrap()
    }

    /// A pairing link for a client of `user` with `role`, as `herder pair` prints it.
    async fn link(&self, user: &str, role: Option<Role>) -> String {
        let request = Request::Pair {
            user: user.into(),
            role,
        };
        let Response::Paired(info) = self.control(request).await else {
            panic!("no pairing code");
        };
        PairingUri {
            hosts: info.addresses,
            fingerprint: info.fingerprint,
            code: info.code,
        }
        .to_string()
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.task.await.unwrap().unwrap();
    }
}

/// Puts one session in a host's journal before its daemon starts.
fn seed(data_dir: &Path, session: &str) {
    std::fs::create_dir_all(data_dir.join("db")).unwrap();
    let mut store = Store::open(data_dir.join("db/herder.db")).unwrap();
    store
        .append(NewEvent {
            session_id: SessionId::new(session),
            at: Timestamp::now(),
            by: None,
            body: EventBody::SessionCreated {
                repo: "/home/dev/app".into(),
                worktree: "/home/dev/worktrees/s1".into(),
                branch: "herder/s1".into(),
                provider: Provider::Claude,
                account_id: AccountId::new("main"),
                model: "m0".into(),
                permission_mode: PermissionMode::Ask,
                parent: None,
                task: None,
                max_children: None,
                failover_pin: None,
            },
        })
        .unwrap();
}

async fn machine_when(client: &Client, ready: impl Fn(&Machine) -> bool) -> Machine {
    let changes = client.changes();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(machine) = client.machines().into_iter().find(&ready) {
            return machine;
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            tokio::time::timeout(left, changes.next()).await.is_ok(),
            "the client never saw it: {:?}",
            client.machines()
        );
    }
}

async fn send(client: &Client, machine: &HostId, command: CommandBody) -> CommandResult {
    tokio::time::timeout(TIMEOUT, client.send(machine.clone(), command))
        .await
        .unwrap()
        .unwrap()
}

async fn refused(client: &Client, machine: &HostId, command: CommandBody) -> ErrorCode {
    match tokio::time::timeout(TIMEOUT, client.send(machine.clone(), command))
        .await
        .unwrap()
    {
        Err(Error::Rejected { info }) => info.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn open(dir: &Path) -> Client {
    Client::open(dir.to_str().unwrap().to_owned(), "herder-test".into()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_links_a_host_to_the_vault_and_unlinks_it() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = Daemon::start(&tmp.path().join("vault"), "", true).await;
    let host_dir = tmp.path().join("host");
    seed(&host_dir.join("data"), "s1");
    let original = "# devbox\n";
    let host = Daemon::start(&host_dir, original, false).await;
    let original = std::fs::read_to_string(&host.config).unwrap();

    // The owner pairs with both; a member pairs with the host.
    let owner = open(&tmp.path().join("owner"));
    let on_vault = owner.pair(vault.link("alice", None).await).await.unwrap();
    let on_host = owner.pair(host.link("alice", None).await).await.unwrap();
    let (vault_id, host_id) = (on_vault.host_id.clone(), on_host.host_id.clone());
    let member = open(&tmp.path().join("member"));
    let link = host.link("bob", Some(Role::Member)).await;
    let as_member = member.pair(link).await.unwrap().host_id;
    machine_when(&owner, |m| {
        m.host_id == host_id && m.role == Some(Role::Owner)
    })
    .await;
    machine_when(&owner, |m| {
        m.host_id == vault_id && m.role == Some(Role::Owner)
    })
    .await;

    // Which machine is a vault, and where the host backs up: nowhere yet.
    assert_eq!(
        send(&owner, &vault_id, CommandBody::GetVaultLink).await,
        CommandResult::VaultLink {
            is_vault: true,
            vault: None
        }
    );
    assert_eq!(
        send(&owner, &host_id, CommandBody::GetVaultLink).await,
        CommandResult::VaultLink {
            is_vault: false,
            vault: None
        }
    );

    // A host code is host-only: a client cannot pair with it.
    let CommandResult::HostPairing { code, .. } = send(
        &owner,
        &vault_id,
        CommandBody::PairVaultHost {
            host_name: "devbox".into(),
        },
    )
    .await
    else {
        panic!("no host code");
    };
    let stranger = open(&tmp.path().join("stranger"));
    let host_code_link = PairingUri {
        hosts: on_vault.addresses.clone(),
        fingerprint: on_vault.fingerprint.clone(),
        code: code.clone(),
    };
    assert!(stranger.pair(host_code_link.to_string()).await.is_err());

    let CommandResult::HostPairing { code, .. } = send(
        &owner,
        &vault_id,
        CommandBody::PairVaultHost {
            host_name: "devbox".into(),
        },
    )
    .await
    else {
        panic!("no host code");
    };
    let linking = CommandBody::LinkVault {
        addresses: on_vault.addresses.clone(),
        fingerprint: on_vault.fingerprint.clone(),
        pairing_code: code,
    };

    // Members can do none of it, on either daemon.
    for (machine, command) in [
        (&as_member, linking.clone()),
        (&as_member, CommandBody::UnlinkVault),
        (&as_member, CommandBody::GetVaultLink),
    ] {
        assert_eq!(
            refused(&member, machine, command).await,
            ErrorCode::Forbidden
        );
    }
    let vault_member = open(&tmp.path().join("vault-member"));
    let link = vault.link("carol", Some(Role::Member)).await;
    let carol_vault = vault_member.pair(link).await.unwrap().host_id;
    for command in [
        CommandBody::PairVaultHost {
            host_name: "devbox".into(),
        },
        CommandBody::RevokeVaultHost {
            host_id: host_id.clone(),
        },
    ] {
        assert_eq!(
            refused(&vault_member, &carol_vault, command).await,
            ErrorCode::Forbidden
        );
    }
    // Nothing replicated meanwhile.
    assert!(owner.machines()[0].sessions.is_empty());

    // Link: the host pairs and replicates at once, with no restart.
    assert_eq!(
        send(&owner, &host_id, linking.clone()).await,
        CommandResult::Applied
    );
    let seen = machine_when(&owner, |m| {
        m.host_id == vault_id
            && m.sessions.iter().any(|s| s.session_id.as_str() == "s1")
            && m.hosts.iter().any(|h| h.host_id == host_id && h.online)
    })
    .await;
    assert_eq!(seen.sessions[0].host_id.as_ref(), Some(&host_id));
    let config = std::fs::read_to_string(&host.config).unwrap();
    assert!(config.starts_with(&original), "{config}");
    assert!(config.contains("[vault]"), "{config}");
    assert!(!config.contains("pairing_code"), "{config}");
    assert_eq!(
        send(&owner, &host_id, CommandBody::GetVaultLink).await,
        CommandResult::VaultLink {
            is_vault: false,
            vault: Some(LinkedVault {
                address: on_vault.addresses[0].clone(),
                fingerprint: on_vault.fingerprint.clone(),
            }),
        }
    );
    // The host paired as a host-only device.
    let Response::Devices { devices } = vault.control(Request::Devices).await else {
        panic!("no devices");
    };
    assert!(
        devices
            .iter()
            .any(|d| d.user == "devbox" && d.device_role == DeviceRole::Host),
        "{devices:?}"
    );
    // A second link waits for the first to stop.
    assert_eq!(
        refused(&owner, &host_id, linking).await,
        ErrorCode::Conflict
    );

    // Stop backing up: replication stops and `[vault]` goes, the rest of the file as it was.
    assert_eq!(
        send(&owner, &host_id, CommandBody::UnlinkVault).await,
        CommandResult::Applied
    );
    machine_when(&owner, |m| {
        m.host_id == vault_id && m.hosts.iter().any(|h| h.host_id == host_id && !h.online)
    })
    .await;
    assert_eq!(std::fs::read_to_string(&host.config).unwrap(), original);
    assert_eq!(
        refused(&owner, &host_id, CommandBody::UnlinkVault).await,
        ErrorCode::NotFound
    );
    // Then the client revokes the host's device on the vault; its sessions stay there.
    let revoke = CommandBody::RevokeVaultHost {
        host_id: host_id.clone(),
    };
    assert_eq!(
        send(&owner, &vault_id, revoke.clone()).await,
        CommandResult::Applied
    );
    let Response::Devices { devices } = vault.control(Request::Devices).await else {
        panic!("no devices");
    };
    assert!(
        devices.iter().all(|d| d.device_role != DeviceRole::Host),
        "{devices:?}"
    );
    assert_eq!(
        refused(&owner, &vault_id, revoke).await,
        ErrorCode::NotFound
    );
    assert!(
        owner
            .machines()
            .iter()
            .any(|m| m.host_id == vault_id && !m.sessions.is_empty())
    );

    drop((owner, member, stranger, vault_member));
    host.stop().await;
    vault.stop().await;
}
