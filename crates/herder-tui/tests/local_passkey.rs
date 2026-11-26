//! A person with no herder app, end to end against the daemon `herder daemon` runs: the
//! terminal claims the machine with a passkey it makes, and approves with it everything an app
//! approves with Face ID.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use herder_client_core::{Client, ConnectionState, Machine};
use herder_daemon::auth::authenticator::Join;
use herder_daemon::auth::control::{self, OwnInfo, Request, Response};
use herder_protocol::{CommandBody, CommandResult, HostId, PersonInfo, Role};
use herder_tui::passkey::{self, Ask, InviteLink, LocalPasskey, PersonOp, Prompted, Secret, Way};
use tokio_util::sync::CancellationToken;

const PASSPHRASE: &str = "correct horse battery";

/// A daemon serving in this process, until dropped.
struct Daemon {
    dir: tempfile::TempDir,
    shutdown: CancellationToken,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Daemon {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("daemon.toml");
        std::fs::write(
            &config_path,
            format!(
                "listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n\n[projects]\ndir = {:?}\n",
                dir.path().join("data"),
                dir.path().join("Projects"),
            ),
        )
        .unwrap();
        let config = herder_daemon::Config::load(Some(&config_path)).unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                herder_daemon::serve(
                    &config,
                    Default::default(),
                    Default::default(),
                    Default::default(),
                    None,
                    shutdown,
                )
                .await
            }
        });
        let daemon = Self { dir, shutdown };
        let socket = daemon.data_dir().join("control.sock");
        until(|| socket.exists().then_some(())).await;
        daemon
    }

    fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// What `herder claim` does: opens the window in which the daemon takes an owner.
    fn claim(&self) -> OwnInfo {
        match control::request(&self.data_dir(), &Request::Claim).unwrap() {
            Response::Own(info) => info,
            _ => panic!("the daemon did not open a claim"),
        }
    }
}

/// Waits until `check` gives something.
async fn until<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "it never happened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn profile(dir: &Path) -> String {
    dir.to_str().unwrap().to_owned()
}

/// The passphrase the user types when asked, and how often they were asked.
#[derive(Default)]
struct Keyboard {
    types: Mutex<Option<String>>,
    asked: AtomicUsize,
}

impl Keyboard {
    fn typing(self: &Arc<Self>, passphrase: Option<&str>) {
        *self.types.lock().unwrap() = passphrase.map(str::to_owned);
    }

    fn ask(self: &Arc<Self>) -> Ask {
        let keyboard = Arc::clone(self);
        Arc::new(move |_| {
            keyboard.asked.fetch_add(1, Ordering::SeqCst);
            let typed = keyboard.types.lock().unwrap().clone();
            Box::pin(async move { typed })
        })
    }
}

/// The TUI's client on the profile `dir`, which has a passkey: opened as `herder_tui::run`
/// opens it, and connected to its one machine.
async fn tui(dir: &Path, keyboard: &Arc<Keyboard>) -> (Client, Arc<Prompted>, HostId) {
    let passkey = LocalPasskey::load(dir).unwrap().expect("a saved passkey");
    let prompted = Arc::new(Prompted::new(passkey, keyboard.ask()));
    let client =
        Client::open_with_passkey(profile(dir), "herder-tui/test".into(), prompted.clone())
            .unwrap();
    let host_id = connected(&client).await.host_id;
    (client, prompted, host_id)
}

async fn connected(client: &Client) -> Machine {
    until(|| {
        client
            .machines()
            .into_iter()
            .find(|machine| machine.connection == ConnectionState::Connected)
    })
    .await
}

async fn people(client: &Client, host_id: &HostId) -> Vec<PersonInfo> {
    match client
        .send(host_id.clone(), CommandBody::ListPeople)
        .await
        .unwrap()
    {
        CommandResult::People { people } => people,
        other => panic!("{other:?}"),
    }
}

/// `herder login '<herder://own link>'` on a terminal with no passkey.
async fn claim_new(daemon: &Daemon, dir: &Path, name: &str) -> Machine {
    let own = daemon.claim();
    let client = Client::open(profile(dir), "herder-cli/test".into()).unwrap();
    let secret = Secret::New {
        name,
        passphrase: PASSPHRASE,
        config_dir: dir,
    };
    passkey::join(
        &client,
        own.addresses,
        own.fingerprint,
        Way::Claim(own.code),
        secret,
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_claims_a_machine_and_approves_with_its_own_passkey() {
    let daemon = Daemon::start().await;
    let laptop = tempfile::tempdir().unwrap();

    // Claim: the terminal makes the passkey, and its person owns the machine.
    assert!(LocalPasskey::load(laptop.path()).unwrap().is_none());
    claim_new(&daemon, laptop.path(), "sam").await;
    let passkey = LocalPasskey::load(laptop.path()).unwrap().unwrap();
    let keyboard = Arc::new(Keyboard::default());
    let (client, prompted, host_id) = tui(laptop.path(), &keyboard).await;
    assert_eq!(connected(&client).await.role, Some(Role::Owner));
    let listed = people(&client, &host_id).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].name.as_str(), listed[0].role),
        ("sam", Role::Owner)
    );
    assert_eq!(listed[0].person_id, *passkey.person_id());
    assert_eq!(listed[0].passkeys.len(), 1);
    assert_eq!(
        listed[0].passkeys[0].credential_id,
        passkey.key().credential_id
    );
    assert_eq!(listed[0].passkeys[0].public_key, passkey.key().public_key);
    let op = |op| passkey::person_op(&client, &prompted, &host_id, op);

    // Nothing is approved with a wrong passphrase, or with none.
    keyboard.typing(Some("correct horse batterz"));
    let refused = op(PersonOp::Invite(Role::Owner)).await.unwrap_err();
    assert_eq!(format!("{refused:#}"), "wrong passphrase");
    keyboard.typing(None);
    let refused = op(PersonOp::Invite(Role::Owner)).await.unwrap_err();
    assert_eq!(format!("{refused:#}"), "cancelled");
    assert_eq!(keyboard.asked.load(Ordering::SeqCst), 2);

    // Roles: an invite brings in the phone they get later, as an app opens its link.
    keyboard.typing(Some(PASSPHRASE));
    let (_, link) = op(PersonOp::Invite(Role::Owner)).await.unwrap();
    let link: InviteLink = link.expect("an invite link").parse().unwrap();
    assert_eq!((link.by.as_deref(), link.role), (Some("sam"), Role::Owner));
    let [invited] = link.machines.as_slice() else {
        panic!("{link:?}");
    };
    let phone_dir = tempfile::tempdir().unwrap();
    let phone = Client::open(profile(phone_dir.path()), "herder-ios/test".into()).unwrap();
    Join::invite(invited.invite.clone())
        .join(&phone, invited.hosts.clone(), &invited.fingerprint, "alice")
        .await;
    let (said, _) = op(PersonOp::People).await.unwrap();
    assert_eq!(said, "sam (owner), alice (owner)");
    op(PersonOp::SetRole("Alice".into(), Role::Member))
        .await
        .unwrap();
    let (said, _) = op(PersonOp::People).await.unwrap();
    assert_eq!(said, "sam (owner), alice (member)");
    assert!(op(PersonOp::Remove("nobody".into())).await.is_err());
    op(PersonOp::Remove("alice".into())).await.unwrap();
    assert_eq!(op(PersonOp::People).await.unwrap().0, "sam (owner)");

    // Adding a device: another terminal runs `herder login`, and this one approves its code.
    let laptop_device = people(&client, &host_id).await[0].devices[0]
        .device_id
        .clone();
    let server_dir = tempfile::tempdir().unwrap();
    let server = Client::open(profile(server_dir.path()), "herder-cli/test".into()).unwrap();
    let (code_tx, code_rx) = tokio::sync::oneshot::channel();
    let logging_in = tokio::spawn({
        let server = server.clone();
        let (hosts, fingerprint) = (invited.hosts.clone(), invited.fingerprint.clone());
        async move {
            server
                .log_in(hosts, fingerprint, move |code, _| {
                    let _ = code_tx.send(code.to_owned());
                })
                .await
        }
    });
    let code = code_rx.await.unwrap();
    op(PersonOp::ApproveLogin(code)).await.unwrap();
    logging_in.await.unwrap().unwrap();
    let sam = &people(&client, &host_id).await[0];
    assert_eq!(sam.devices.len(), 2);

    // That terminal has no passkey: what it does that needs one, this one is asked to
    // approve, and refuses or approves.
    connected(&server).await;
    let asking = |client: &Client| {
        let (client, host_id) = (client.clone(), host_id.clone());
        // A device no daemon has.
        let device_id = herder_protocol::DeviceId::new(format!("{laptop_device}-gone"));
        tokio::spawn(async move {
            let revoke = CommandBody::RevokeDevice {
                device_id,
                assertion: None,
            };
            client.send(host_id, revoke).await
        })
    };
    let asked = asking(&server);
    let request = until(|| connected_now(&client)?.approvals.first().cloned()).await;
    op(PersonOp::RefuseAction(request.request_id))
        .await
        .unwrap();
    assert!(asked.await.unwrap().is_err());
    let asked = asking(&server);
    let request = until(|| connected_now(&client)?.approvals.first().cloned()).await;
    let before = keyboard.asked.load(Ordering::SeqCst);
    op(PersonOp::ApproveAction(request.request_id))
        .await
        .unwrap();
    assert_eq!(keyboard.asked.load(Ordering::SeqCst), before + 1);
    // Approved, it ran: the daemon looked for the device, which there is none of.
    let answer = asked.await.unwrap().unwrap_err();
    assert!(format!("{answer}").contains("device"), "{answer}");

    // What this terminal sends that needs a passkey, it signs itself: nobody's phone is
    // asked. Here it unpairs the other terminal.
    let other = sam
        .devices
        .iter()
        .find(|device| device.device_id != laptop_device)
        .unwrap()
        .device_id
        .clone();
    let revoke = CommandBody::RevokeDevice {
        device_id: other,
        assertion: None,
    };
    keyboard.typing(Some("not the passphrase"));
    assert!(client.send(host_id.clone(), revoke.clone()).await.is_err());
    assert_eq!(people(&client, &host_id).await[0].devices.len(), 2);
    keyboard.typing(Some(PASSPHRASE));
    client.send(host_id.clone(), revoke).await.unwrap();
    assert_eq!(people(&client, &host_id).await[0].devices.len(), 1);
}

fn connected_now(client: &Client) -> Option<Machine> {
    client
        .machines()
        .into_iter()
        .find(|machine| machine.connection == ConnectionState::Connected)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_passkey_owns_a_second_machine_and_signs_in_new_terminals() {
    let first = Daemon::start().await;
    let second = Daemon::start().await;
    let laptop = tempfile::tempdir().unwrap();
    let machine = claim_new(&first, laptop.path(), "sam").await;
    let passkey = LocalPasskey::load(laptop.path()).unwrap().unwrap();

    // The second machine's claim takes the passkey the terminal has, by its passphrase.
    let client = Client::open(profile(laptop.path()), "herder-cli/test".into()).unwrap();
    let claim = |passphrase: &'static str| {
        let own = second.claim();
        passkey::join(
            &client,
            own.addresses,
            own.fingerprint,
            Way::Claim(own.code),
            Secret::Existing(&passkey, passphrase),
        )
    };
    let refused = claim("not the passphrase").await.unwrap_err();
    assert_eq!(format!("{refused:#}"), "wrong passphrase");
    let other = claim(PASSPHRASE).await.unwrap();
    assert_ne!(other.host_id, machine.host_id);
    let keyboard = Arc::new(Keyboard::default());
    let prompted = Arc::new(Prompted::new(passkey.clone(), keyboard.ask()));
    let tui = Client::open_with_passkey(
        profile(laptop.path()),
        "herder-tui/test".into(),
        prompted.clone(),
    )
    .unwrap();
    let both = until(|| {
        let machines = tui.machines();
        machines
            .iter()
            .all(|machine| machine.connection == ConnectionState::Connected)
            .then_some(machines)
    })
    .await;
    assert_eq!(both.len(), 2);
    for machine in &both {
        assert_eq!(machine.role, Some(Role::Owner));
        assert_eq!(machine.person_id.as_ref(), Some(passkey.person_id()));
    }

    // A copy of the profile's passkey on another terminal signs in to a machine that knows
    // the person, with the passphrase and not without.
    let desktop = tempfile::tempdir().unwrap();
    let copied = Client::open(profile(desktop.path()), "herder-cli/test".into()).unwrap();
    let own = first.claim();
    let sign_in = |passphrase: &'static str| {
        passkey::sign_in(
            &copied,
            own.addresses.clone(),
            own.fingerprint.clone(),
            &passkey,
            passphrase,
        )
    };
    let refused = sign_in("").await.unwrap_err();
    assert_eq!(format!("{refused:#}"), "wrong passphrase");
    assert!(copied.machines().is_empty());
    let signed_in = sign_in(PASSPHRASE).await.unwrap();
    assert_eq!(signed_in.host_id, machine.host_id);

    // The TUI's add-machine dialog signs in the same way, asking for the passphrase.
    let tablet = tempfile::tempdir().unwrap();
    let added = Client::open(profile(tablet.path()), "herder-tui/test".into()).unwrap();
    let add = || prompted.sign_in(&added, own.addresses.clone(), own.fingerprint.clone());
    keyboard.typing(Some("not the passphrase"));
    assert!(add().await.is_err());
    keyboard.typing(Some(PASSPHRASE));
    assert_eq!(add().await.unwrap().host_id, machine.host_id);
}
