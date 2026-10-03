//! The bindings as Swift and Kotlin drive them: futures polled from a thread with no tokio
//! runtime, against a daemon with the fake adapter.

mod support;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use herder_ffi::{
    Client, HerderError, image_media_types, max_image_bytes, max_prompt_image_bytes,
    pairing_uri_to_string, parse_pairing_uri,
};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, DirectoryEntry, EventBody, ItemBody, PermissionMode,
    SessionStatus,
};
use support::{ACCOUNT, FakeDaemon};

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

#[test]
fn a_client_pairs_and_streams_a_session_without_a_runtime_of_its_callers() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let daemon = runtime.block_on(FakeDaemon::start()).unwrap();
    let config = tempfile::tempdir().unwrap();

    let link = parse_pairing_uri(daemon.link.clone()).unwrap();
    assert_eq!(pairing_uri_to_string(link), daemon.link);
    let client = Client::open(
        config.path().display().to_string(),
        "herder-ffi-test/0".into(),
    )
    .unwrap();
    let machine = block_on(client.pair(daemon.link.clone())).unwrap();
    assert_eq!(machine.name, "fake-host");
    let host = machine.host_id;
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
            max_children: None,
            failover_pin: None,
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

    // Backgrounded and back, the client syncs again.
    client.suspend();
    client.wake();
    block_on(client.synced(host)).unwrap();

    drop(subscription);
    drop(client);
    runtime.block_on(daemon.stop()).unwrap();
}

#[test]
fn the_image_limits_are_the_protocols() {
    assert_eq!(image_media_types(), herder_protocol::IMAGE_MEDIA_TYPES);
    assert_eq!(max_image_bytes(), herder_protocol::MAX_IMAGE_BYTES as u64);
    assert_eq!(
        max_prompt_image_bytes(),
        herder_protocol::MAX_PROMPT_IMAGE_BYTES as u64
    );
}

#[test]
fn an_invalid_link_fails_with_invalid_link() {
    let error = parse_pairing_uri("https://example.com".into()).unwrap_err();
    assert!(
        matches!(error, HerderError::InvalidLink { .. }),
        "{error:?}"
    );
}
