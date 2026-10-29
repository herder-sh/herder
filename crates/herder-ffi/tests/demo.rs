//! The demo fleet of the site's screenshots (`fake_daemon --demo`) sets up every scene the
//! screenshots show.

#[path = "../examples/fake_daemon/demo.rs"]
mod demo;

use herder_protocol::{AccountId, EventBody, ItemBody, Role, SessionStatus};

#[tokio::test(flavor = "multi_thread")]
async fn the_demo_sets_up_every_scene() -> anyhow::Result<()> {
    let demo = demo::Demo::start(false).await?;
    assert!(demo.link.starts_with("herder://pair?"), "{}", demo.link);
    let scenes = demo.scenes().await?;
    let scene = |host: &str, title: &str| {
        scenes
            .iter()
            .find(|scene| {
                scene.host == host
                    && scene.title == title
                    && scene.status != SessionStatus::Archived
            })
            .unwrap_or_else(|| panic!("no session {title:?} on {host}"))
    };
    let count = |scene: &demo::Scene, what: fn(&EventBody) -> bool| {
        scene.events.iter().filter(|body| what(body)).count()
    };

    // Claude subagents at work.
    let agents = scene("devbox", "Audit webhook retries");
    assert_eq!(agents.status, SessionStatus::Running);
    let agent = |body: &EventBody| {
        matches!(body, EventBody::ItemAdded { item } if item.parent_call_id.is_none()
            && matches!(&item.body, ItemBody::ToolCall { name, .. } if name == "Agent"))
    };
    assert_eq!(count(agents, agent), 3);

    // An approval.
    let dates = scene("devbox", "Upgrade date-fns to v4");
    assert_eq!(dates.status, SessionStatus::NeedsYou);

    // A question several of whose choices may be picked.
    let browsers = scene("laptop", "Run checkout e2e in more browsers");
    assert_eq!(browsers.status, SessionStatus::NeedsYou);
    assert_eq!(
        count(browsers, |body| matches!(
            body,
            EventBody::QuestionAsked {
                multi_select: true,
                ..
            }
        )),
        1
    );

    // Failover: the limit on Work, the switch to Personal, the retried turn done there.
    let ledger = scene("devbox", "Move invoices onto the ledger");
    let failed = ledger
        .events
        .iter()
        .position(|body| matches!(body, EventBody::TurnFailed { .. }))
        .expect("the turn hit the limit");
    let switched = ledger
        .events
        .iter()
        .position(|body| {
            *body
                == EventBody::AccountSwitched {
                    account_id: AccountId::new("claude-personal"),
                }
        })
        .expect("the session failed over");
    assert!(failed < switched);
    assert_eq!(ledger.status, SessionStatus::Idle);
    assert_eq!(
        count(ledger, |body| matches!(
            body,
            EventBody::TurnCompleted { .. }
        )),
        1
    );

    // A task tree: three children, one of which reported.
    let retention = scene("devbox", "Roll out 30-day log retention");
    assert_eq!(
        count(retention, |body| matches!(
            body,
            EventBody::ChildSpawned { .. }
        )),
        3
    );
    assert!(
        count(retention, |body| matches!(
            body,
            EventBody::ChildReported { .. }
        )) >= 1
    );

    // A handoff from devbox onto studio, finished there; the original is put away.
    let fork = scene("studio", "Fix the flaky checkout e2e test");
    assert!(count(fork, |body| matches!(body, EventBody::SessionForked { .. })) == 1);
    assert_eq!(fork.status, SessionStatus::Idle);
    assert!(scenes.iter().any(|scene| scene.host == "devbox"
        && scene.title == "Fix the flaky checkout e2e test"
        && scene.status == SessionStatus::Archived));

    // Conversations people started from Slack through devbox's bots, one waiting on an
    // approval.
    let rollback = scene("devbox", "Roll back the staging deploy");
    assert_eq!(rollback.status, SessionStatus::NeedsYou);
    assert_eq!(
        rollback
            .origin
            .as_ref()
            .map(|origin| origin.bot_id.as_str()),
        Some("acme-ops")
    );
    let bots = scenes.iter().filter(|scene| scene.origin.is_some()).count();
    assert_eq!(bots, 4);

    // The People page's team: the user owns devbox with Sam and Kim as members, and is a
    // member on laptop, which Pat and Kim own.
    let team = |host: &str| {
        let mut users: Vec<(String, Role)> = demo
            .users(host)
            .expect("the machine's users")
            .into_iter()
            .map(|user| (user.name, user.role))
            .collect();
        users.sort_by(|a, b| a.0.cmp(&b.0));
        users
    };
    let named = |users: &[(&str, Role)]| {
        users
            .iter()
            .map(|(name, role)| ((*name).to_owned(), *role))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        team("devbox"),
        named(&[
            ("kim", Role::Member),
            ("sam", Role::Member),
            ("tomas", Role::Owner)
        ])
    );
    assert_eq!(
        team("laptop"),
        named(&[
            ("kim", Role::Owner),
            ("pat", Role::Owner),
            ("sam", Role::Member),
            ("tomas", Role::Member)
        ])
    );

    // Every machine has work on it.
    for host in ["devbox", "studio", "laptop"] {
        assert!(
            scenes.iter().any(|scene| scene.host == host),
            "nothing on {host}"
        );
    }
    demo.stop().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_busy_demo_adds_a_long_transcript_that_keeps_streaming() -> anyhow::Result<()> {
    let demo = demo::Demo::start(true).await?;
    let scenes = demo.scenes().await?;
    let busy = scenes
        .iter()
        .find(|scene| scene.host == "devbox" && scene.title == "A long transcript, still streaming")
        .expect("no busy session on devbox");
    assert_eq!(busy.status, SessionStatus::Running);
    let finished = busy
        .events
        .iter()
        .filter(|body| matches!(body, EventBody::TurnCompleted { .. }))
        .count();
    assert_eq!(finished, 100);
    // About 2,000 items, as the apps are profiled scrolling.
    let items = busy
        .events
        .iter()
        .filter(|body| matches!(body, EventBody::ItemAdded { .. }))
        .count();
    assert!(items >= 2000, "{items} items");
    demo.stop().await
}
