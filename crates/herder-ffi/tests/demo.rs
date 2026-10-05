//! The demo fleet of the site's screenshots (`fake_daemon --demo`) sets up every scene the
//! screenshots show.

#[path = "../examples/fake_daemon/demo.rs"]
mod demo;

use herder_protocol::{AccountId, EventBody, ItemBody, SessionStatus};

#[tokio::test(flavor = "multi_thread")]
async fn the_demo_sets_up_every_scene() -> anyhow::Result<()> {
    let demo = demo::Demo::start().await?;
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

    // Every machine has work on it.
    for host in ["devbox", "studio", "laptop"] {
        assert!(
            scenes.iter().any(|scene| scene.host == host),
            "nothing on {host}"
        );
    }
    demo.stop().await
}
