//! A demo: a recorded machine shows as connected, its session streams the recorded events, a
//! recorded command gets its answer, and everything else fails without reaching anything.

use std::collections::HashMap;

use herder_client_core::{
    Client, ConnectionQuality, ConnectionState, Demo, DemoAnswer, DemoMachine, Error, Machine,
};
use herder_protocol::{
    AccountId, CommandBody, CommandResult, Event, EventBody, FailoverSettings, HostId,
    PermissionMode, Provider, Role, SessionId, Timestamp,
};

fn demo() -> Demo {
    let machine = Machine {
        host_id: HostId::new("devbox"),
        name: "devbox".into(),
        addresses: vec!["127.0.0.1:7300".into()],
        address: Some("127.0.0.1:7300".into()),
        fingerprint: "00".into(),
        connection: ConnectionState::Connected,
        quality: ConnectionQuality::default(),
        role: Some(Role::Owner),
        person_id: None,
        user_id: None,
        herder_version: Some("0.6.0".into()),
        sessions: Vec::new(),
        hosts: Vec::new(),
        projects: Vec::new(),
        accounts: Vec::new(),
        failover: FailoverSettings::default(),
        providers: Vec::new(),
        terminals: Vec::new(),
        resources: None,
        session_usage: HashMap::new(),
        vault: None,
        skills: None,
        session_skills: HashMap::new(),
        update: None,
        approvals: Vec::new(),
    };
    let created = Event {
        session_id: SessionId::new("s1"),
        seq: 1,
        at: Timestamp::UNIX_EPOCH,
        by: None,
        body: EventBody::SessionCreated {
            repo: "/src/api".into(),
            worktree: "/src/api".into(),
            branch: None,
            provider: Provider::Claude,
            account_id: AccountId::new("claude-personal"),
            model: "opus".into(),
            permission_mode: PermissionMode::Ask,
            parent: None,
            parent_host: None,
            task: None,
            failover_pin: None,
            chat: false,
            origin: None,
        },
    };
    Demo {
        machines: vec![DemoMachine {
            machine,
            logs: vec![vec![created]],
            answers: vec![DemoAnswer {
                command: CommandBody::ListPeople,
                result: CommandResult::People { people: Vec::new() },
            }],
        }],
    }
}

#[tokio::test]
async fn a_demo_shows_what_was_recorded_and_sends_nothing() {
    let recorded = demo();
    // What the apps bundle is the JSON of a demo.
    let json = serde_json::to_string(&recorded).unwrap();
    let client = Client::open_demo(serde_json::from_str(&json).unwrap());
    let host = HostId::new("devbox");

    let machines = client.machines();
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].connection, ConnectionState::Connected);
    assert_eq!(machines[0].role, Some(Role::Owner));

    let session = client
        .subscribe_session(host.clone(), SessionId::new("s1"))
        .unwrap();
    let update = session.next().await.unwrap();
    assert_eq!(update.events, recorded.machines[0].logs[0]);

    assert_eq!(
        client
            .send(host.clone(), CommandBody::ListPeople)
            .await
            .unwrap(),
        CommandResult::People { people: Vec::new() }
    );
    assert_eq!(
        client.send(host.clone(), CommandBody::ListBots).await,
        Err(Error::Demo)
    );
    assert_eq!(client.synced(host.clone()).await, Ok(()));
    assert_eq!(client.reconnect(host.clone()).await, Err(Error::Demo));
    assert_eq!(client.rename(host.clone(), "box".into()), Err(Error::Demo));
    assert_eq!(client.forget(host), Err(Error::Demo));
}
