//! The bindings as Swift and Kotlin drive them: futures polled from a thread with no tokio
//! runtime, against a daemon with the fake adapter.

mod support;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use herder_client_core::Machine;
use herder_daemon::auth::authenticator::Join;
use herder_ffi::{
    Client, HerderError, fleet_link_to_string, image_media_types, max_file_bytes,
    max_file_name_bytes, max_image_bytes, max_project_icon_bytes, max_prompt_attachment_bytes,
    parse_fleet_link,
};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, DirectoryEntry, EventBody, ItemBody, McpServerSettings,
    McpTransport, PermissionMode, Provider, Role, SessionSkill, SessionStatus, SkillSource,
    UsagePeriod, UsageTotal,
};
use support::{ACCOUNT, DEMO_SKILLS, FakeDaemon, IMPORTED_SKILL, PROJECT_SKILL};

const TIMEOUT: Duration = Duration::from_secs(20);

/// Runs a future to completion on this thread, as a foreign executor would: no tokio runtime.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no result in time");
        thread::park_timeout(left);
    }
}

/// Signs `client` in to `daemon`, where its link points, as the new person `name` with `role`,
/// through the bindings.
async fn sign_in(daemon: &FakeDaemon, client: &Client, name: &str, role: Role) -> Machine {
    let join = Join::open(&daemon.auth, role);
    let at = parse_fleet_link(daemon.link.clone())
        .unwrap()
        .machines
        .remove(0);
    let pending = client.sign_in(at.hosts, at.fingerprint).await.unwrap();
    let (_, sign_in) = join.sign_in(name, &pending.registration().unwrap());
    pending.finish(sign_in).await.unwrap()
}

#[test]
fn a_client_signs_in_and_streams_a_session_without_a_runtime_of_its_callers() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let daemon = runtime.block_on(FakeDaemon::start("fake-host")).unwrap();
    let config = tempfile::tempdir().unwrap();

    let link = parse_fleet_link(daemon.link.clone()).unwrap();
    assert_eq!(fleet_link_to_string(link), daemon.link);
    let client = Client::open(
        config.path().display().to_string(),
        "herder-ffi-test/0".into(),
    )
    .unwrap();
    let machine = block_on(sign_in(&daemon, &client, "alice", Role::Owner));
    assert_eq!(machine.name, "fake-host");
    let host = machine.host_id.clone();
    block_on(client.synced(host.clone())).unwrap();
    // The connection's first ping goes out as soon as it is up; its round trip comes through.
    let changes = client.changes();
    while client.machines()[0].quality.last_rtt_ms.is_none() {
        assert!(block_on(changes.next()));
    }
    assert!(client.machines()[0].quality.connected_since.is_some());

    let created = block_on(client.send(
        host.clone(),
        CommandBody::CreateSession {
            repo: Some(daemon.repo.clone()),
            project_id: None,
            branch: None,
            account_id: Some(AccountId::new(ACCOUNT)),
            provider: None,
            model: None,
            permission_mode: Some(PermissionMode::Ask),
            failover_pin: None,
            chat: false,
        },
    ))
    .unwrap();
    let CommandResult::SessionCreated { session_id } = created else {
        panic!("expected a session, got {created:?}");
    };
    let subscription = client
        .subscribe_session(host.clone(), session_id.clone())
        .unwrap();
    let sent = block_on(client.send(
        host.clone(),
        CommandBody::SendPrompt {
            session_id,
            text: "Say hello.".into(),
            images: Vec::new(),
            files: Vec::new(),
        },
    ))
    .unwrap();
    assert_eq!(sent, CommandResult::Applied);

    let mut events = Vec::new();
    while !events.iter().any(|body| {
        *body
            == EventBody::SessionStatusChanged {
                retry_at: None,
                status: SessionStatus::Idle,
            }
    }) || !events
        .iter()
        .any(|body| matches!(body, EventBody::TurnCompleted { .. }))
    {
        let update = block_on(subscription.next()).expect("the subscription ended");
        events.extend(update.events.into_iter().map(|event| event.body));
    }
    assert!(
        events.iter().any(|body| matches!(
            body,
            EventBody::ItemAdded { item } if item.body == ItemBody::AssistantMessage {
                text: "Hello, world.".into()
            }
        )),
        "{events:?}"
    );

    // The turn's usage adds up on the daemon, for its owner and for a member alike.
    let summary = |client: &Client| match block_on(client.send(
        host.clone(),
        CommandBody::GetUsageSummary {
            period: UsagePeriod::Day,
        },
    ))
    .unwrap()
    {
        CommandResult::UsageSummary { totals, .. } => totals,
        other => panic!("expected a usage summary, got {other:?}"),
    };
    let totals = summary(&client);
    assert_eq!(
        totals,
        [UsageTotal {
            account_id: AccountId::new(ACCOUNT),
            provider: Provider::Other(ACCOUNT.into()),
            model: String::new(),
            turns: 1,
            input: 1_200,
            output: 340,
            cache_read: 18_000,
            cache_write: 2_048,
            cost_usd: 0.0425,
            cost_estimated: false,
        }]
    );
    let guest_config = tempfile::tempdir().unwrap();
    let guest = Client::open(
        guest_config.path().display().to_string(),
        "herder-ffi-test/0".into(),
    )
    .unwrap();
    block_on(sign_in(&daemon, &guest, "guest", Role::Member));
    block_on(guest.synced(host.clone())).unwrap();
    assert_eq!(guest.machines()[0].role, Some(Role::Member));
    assert_eq!(summary(&guest), totals);

    // A query's answer comes back as the command's result.
    let repo = std::path::Path::new(&daemon.repo);
    let listed = block_on(client.send(
        host.clone(),
        CommandBody::ListDirectory {
            path: repo.parent().unwrap().display().to_string(),
        },
    ))
    .unwrap();
    let CommandResult::Directory { entries, .. } = listed else {
        panic!("expected a listing, got {listed:?}");
    };
    let name = repo.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        entries.contains(&DirectoryEntry {
            name,
            is_dir: true,
            is_repo: true,
        }),
        "{entries:?}"
    );

    // A daemon is no vault, and refuses to fork a session it cannot find.
    let machine = client.machines().into_iter().next().unwrap();
    assert_eq!(machine.vault, None);
    let refused = block_on(client.send(
        host.clone(),
        CommandBody::ForkSession {
            session_id: herder_protocol::SessionId::new("gone"),
            account_id: None,
            relay: None,
        },
    ))
    .unwrap_err();
    let HerderError::Rejected { info } = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert_eq!(info.code, herder_protocol::ErrorCode::NotFound);
    assert!(info.message.contains("gone"), "{}", info.message);

    // The daemon serves its demo library, and the session sees the project skill checked in
    // to the repository; the skill commands pass through.
    let skills = machine.skills.clone().expect("the daemon has a library");
    assert!(skills.repo.is_some() && skills.head.is_some() && skills.last_pull.is_some());
    assert_eq!(skills.pull_error, None);
    assert_eq!(
        skills
            .skills
            .iter()
            .map(|skill| (
                skill.name.as_str(),
                skill.description.as_str(),
                skill.enabled
            ))
            .collect::<Vec<_>>(),
        DEMO_SKILLS
            .iter()
            .map(|(name, description)| (*name, *description, true))
            .collect::<Vec<_>>()
    );
    let listed = machine.session_skills.values().next().cloned();
    assert_eq!(
        listed,
        Some(vec![SessionSkill {
            name: PROJECT_SKILL.0.into(),
            description: PROJECT_SKILL.1.into(),
            source: SkillSource::Project,
            path: Some(".claude/skills/deploy".into()),
        }])
    );
    let imported = block_on(client.send(
        host.clone(),
        CommandBody::ImportSkill {
            git_url: daemon.skill_source.clone(),
            path: Some(IMPORTED_SKILL.0.into()),
        },
    ))
    .unwrap();
    assert_eq!(imported, CommandResult::Applied);
    while !client.machines()[0].skills.as_ref().is_some_and(|skills| {
        skills
            .skills
            .iter()
            .any(|skill| skill.name == IMPORTED_SKILL.0)
    }) {
        assert!(block_on(changes.next()));
    }

    // An owner reads the MCP servers and adds one, which applies without a restart.
    let read = |client: &Client| match block_on(client.send(host.clone(), CommandBody::GetSettings))
        .unwrap()
    {
        CommandResult::Settings {
            settings,
            restart_required,
            ..
        } => (*settings, restart_required),
        other => panic!("expected settings, got {other:?}"),
    };
    let (mut settings, _) = read(&client);
    assert!(settings.mcp_servers.is_empty());
    settings.mcp_servers.push(McpServerSettings {
        name: "docs".into(),
        enabled: true,
        project: None,
        transport: McpTransport::Stdio {
            command: "docs-mcp".into(),
            args: vec!["--stdio".into()],
            env: [("DOCS_HOST".to_owned(), "docs.example.com".to_owned())].into(),
            secrets: vec!["DOCS_TOKEN".parse().unwrap()],
        },
    });
    block_on(client.send(
        host.clone(),
        CommandBody::SetSettings {
            settings: Box::new(settings.clone()),
        },
    ))
    .unwrap();
    assert_eq!(read(&client), (settings, false));

    // Backgrounded and back, the client syncs again.
    client.suspend();
    client.wake();
    block_on(client.synced(host)).unwrap();

    drop(subscription);
    drop(client);
    runtime.block_on(daemon.stop()).unwrap();
}

#[test]
fn the_attachment_limits_are_the_protocols() {
    assert_eq!(image_media_types(), herder_protocol::IMAGE_MEDIA_TYPES);
    assert_eq!(max_image_bytes(), herder_protocol::MAX_IMAGE_BYTES as u64);
    assert_eq!(max_file_bytes(), herder_protocol::MAX_FILE_BYTES as u64);
    assert_eq!(
        max_file_name_bytes(),
        herder_protocol::MAX_FILE_NAME_BYTES as u64
    );
    assert_eq!(
        max_prompt_attachment_bytes(),
        herder_protocol::MAX_PROMPT_ATTACHMENT_BYTES as u64
    );
    assert_eq!(
        max_project_icon_bytes(),
        herder_protocol::MAX_PROJECT_ICON_BYTES as u64
    );
}

#[test]
fn an_invalid_link_fails_with_invalid_link() {
    let error = parse_fleet_link("https://example.com".into()).unwrap_err();
    assert!(
        matches!(error, HerderError::InvalidLink { .. }),
        "{error:?}"
    );
}
