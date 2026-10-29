use herder_protocol::{ErrorCode, Event, EventBody, QueuedPrompt, SessionId};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::ws::Identity;

/// A vault as the claim finds it, recorded from a real one's `auth.json` with ids and
/// fingerprints made up: `tomas`, its owner, with a phone; `owner`, a user from before with a
/// CLI device; `trash-can-01`, a machine paired as a user, which replicates as a client to take
/// handoffs; `draugelis`, a machine paired with `herder pair --host`; and `bob`, a member.
const VAULT: &str = include_str!("fixtures/vault-auth.json");

struct Vault {
    tmp: tempfile::TempDir,
    auth: Auth,
    tomas: Identity,
    owner: Identity,
    machine: Identity,
    bob: Identity,
}

fn vault() -> Vault {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(super::super::FILE), VAULT).unwrap();
    let auth = Auth::open(tmp.path()).unwrap();
    let tomas = connect(&auth, "fp-phone").unwrap();
    let owner = connect(&auth, "fp-cli").unwrap();
    let machine = connect(&auth, "fp-trash-can").unwrap();
    let bob = connect(&auth, "fp-bob").unwrap();
    assert_eq!(tomas.role, Role::Owner);
    Vault {
        tmp,
        auth,
        tomas,
        owner,
        machine,
        bob,
    }
}

const PERSON: &str = "0192f3a4-5b6c-7d8e-9f00-112233445566";

/// The person's id, as their connections act.
fn person() -> UserId {
    UserId::new(PERSON)
}

/// Claims `auth` from `caller` as the person, who holds no passkey in these tests.
fn claim(
    auth: &Auth,
    caller: &UserId,
    also: &[UserId],
    replicated: &[DeviceId],
) -> Result<Claimed, ErrorInfo> {
    auth.claim_as(
        caller,
        &PersonId::new(PERSON),
        "Tomas".into(),
        Vec::new(),
        also,
        replicated,
    )
}

fn connect(auth: &Auth, fingerprint: &str) -> Result<Identity, ErrorInfo> {
    auth.authenticate(
        fingerprint,
        None,
        "test",
        DeviceRole::Client,
        &CancellationToken::new(),
    )
}

fn event(by: &UserId) -> ServerMessage {
    ServerMessage::Event(Event {
        session_id: SessionId::new("s1"),
        seq: 1,
        at: Timestamp::now(),
        by: Some(by.clone()),
        body: EventBody::Unknown,
    })
}

fn by(message: &ServerMessage) -> Option<&UserId> {
    match message {
        ServerMessage::Event(event) => event.by.as_ref(),
        _ => None,
    }
}

#[test]
fn a_claim_moves_the_owner_to_the_person_makes_machines_hosts_and_leaves_the_rest() {
    let v = vault();
    // A host the vault knows from its replication: every device of the machine-user is one.
    let replicated = [v.machine.device_id.clone()];
    let elsewhere = UserId::new("01JA0HOSTUSER");
    let claimed = claim(
        &v.auth,
        &v.tomas.user_id,
        std::slice::from_ref(&elsewhere),
        &replicated,
    )
    .unwrap();
    // So is draugelis's, a host-only device.
    assert_eq!(claimed.hosts, ["trash-can-01", "draugelis"]);
    let left: Vec<_> = claimed.leftovers.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(left, ["owner", "bob"]);
    assert_eq!(v.auth.leftovers(), claimed.leftovers);

    // The owner's phone is the person's now, an owner still.
    let phone = connect(&v.auth, "fp-phone").unwrap();
    assert_eq!((&phone.user_id, phone.role), (&person(), Role::Owner));
    assert_eq!(v.auth.owner(), Some(person()));

    // Its old id, and the person's id on another machine, resolve to the person.
    for old in [&v.tomas.user_id, &elsewhere] {
        assert_eq!(v.auth.resolve(old), person());
        assert_eq!(v.auth.role(old), Some(Role::Owner));
        assert_eq!(v.auth.name(old).as_deref(), Some("Tomas"));
        let mut message = event(old);
        v.auth.attribute(&mut message);
        assert_eq!(by(&message), Some(&person()));
    }
    // Others' history keeps its author.
    let mut message = event(&v.owner.user_id);
    v.auth.attribute(&mut message);
    assert_eq!(by(&message), Some(&v.owner.user_id));

    // The machine is listed with the hosts, not the users, and still replicates.
    let users: Vec<_> = v.auth.users().into_iter().map(|u| u.name).collect();
    assert_eq!(users, ["owner", "bob"], "the owner is a person now");
    assert!(connect(&v.auth, "fp-trash-can").is_ok());
    let draugelis = v.auth.authenticate(
        "fp-draugelis",
        None,
        "test",
        DeviceRole::Host,
        &CancellationToken::new(),
    );
    assert!(draugelis.is_ok());
}

#[test]
fn queued_prompts_read_as_the_person() {
    let v = vault();
    claim(&v.auth, &v.tomas.user_id, &[], &[]).unwrap();
    let head: herder_protocol::SessionHead = serde_json::from_value(serde_json::json!({
        "session_id": "s1",
        "head_seq": 3,
        "status": "running",
        "account_id": "work",
        "children_need_you": 0,
    }))
    .unwrap();
    let prompt = |by: &UserId| QueuedPrompt {
        prompt_id: herder_protocol::PromptId::new("p1"),
        text: "ship it".into(),
        images: 0,
        files: 0,
        by: Some(by.clone()),
        agent_message: None,
    };
    let mut message = ServerMessage::Sessions {
        sessions: vec![herder_protocol::SessionHead {
            queue: vec![prompt(&v.tomas.user_id), prompt(&v.owner.user_id)],
            ..head
        }],
    };
    v.auth.attribute(&mut message);
    let ServerMessage::Sessions { sessions } = message else {
        unreachable!()
    };
    let by: Vec<_> = sessions[0].queue.iter().map(|p| p.by.clone()).collect();
    assert_eq!(by, [Some(person()), Some(v.owner.user_id.clone())]);
}

#[test]
fn the_claim_survives_a_restart() {
    let v = vault();
    let replicated = [v.machine.device_id.clone()];
    claim(&v.auth, &v.tomas.user_id, &[], &replicated).unwrap();
    let auth = Auth::open(v.tmp.path()).unwrap();
    assert_eq!(auth.resolve(&v.tomas.user_id), person());
    assert_eq!(
        connect(&auth, "fp-phone").unwrap().user_id,
        person(),
        "the phone is the person's"
    );
    let left: Vec<_> = auth.leftovers().into_iter().map(|u| u.name).collect();
    assert_eq!(left, ["owner", "bob"]);
}

#[test]
fn only_an_owner_claims_and_only_for_one_person() {
    let v = vault();
    let bob = &v.bob;
    assert_eq!(bob.role, Role::Member);
    let refused = claim(&v.auth, &bob.user_id, &[], &[]).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(v.auth.leftovers().is_empty(), "nothing moved");

    claim(&v.auth, &v.tomas.user_id, &[], &[]).unwrap();
    // Again, as the same person, from the old id or the new: more ids, nothing else.
    let more = UserId::new("01JA0MORE");
    claim(&v.auth, &v.tomas.user_id, std::slice::from_ref(&more), &[]).unwrap();
    assert_eq!(v.auth.resolve(&more), person());
    let other = UserId::new("0192f3a4-0000-7000-8000-000000000000");
    let conflict = v
        .auth
        .claim_as(
            &person(),
            &PersonId::new(other.as_str()),
            "Someone".into(),
            Vec::new(),
            &[],
            &[],
        )
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::Conflict);
}

#[test]
fn a_leftover_is_the_person_or_removed() {
    let v = vault();
    let bob = {
        let connection = CancellationToken::new();
        let bob = v
            .auth
            .authenticate("fp-bob", None, "test", DeviceRole::Client, &connection)
            .unwrap();
        (bob, connection)
    };
    let cli = CancellationToken::new();
    v.auth
        .authenticate("fp-cli", None, "test", DeviceRole::Client, &cli)
        .unwrap();
    claim(&v.auth, &v.tomas.user_id, &[], &[]).unwrap();
    let left: Vec<_> = v.auth.leftovers().into_iter().map(|u| u.name).collect();
    assert_eq!(left, ["owner", "trash-can-01", "bob"]);

    // "It's me": the CLI joins the person, as an owner, and its history reads as theirs.
    v.auth
        .claim_leftover(&v.owner.user_id, &PersonId::new(PERSON))
        .unwrap();
    assert!(cli.is_cancelled(), "it connects again as the person");
    let again = connect(&v.auth, "fp-cli").unwrap();
    assert_eq!((&again.user_id, again.role), (&person(), Role::Owner));
    assert_eq!(v.auth.resolve(&v.owner.user_id), person());

    // "Remove": bob and his devices go; his history keeps his id.
    let (bob, connection) = bob;
    v.auth.remove_user(&bob.user_id).unwrap();
    assert!(connection.is_cancelled());
    assert_eq!(
        connect(&v.auth, "fp-bob").unwrap_err().code,
        ErrorCode::Forbidden
    );
    assert_eq!(v.auth.resolve(&bob.user_id), bob.user_id);

    let left: Vec<_> = v.auth.leftovers().into_iter().map(|u| u.name).collect();
    assert_eq!(left, ["trash-can-01"]);
    // The person and anyone gone are no leftovers.
    for decided in [&person(), &bob.user_id] {
        assert!(v.auth.remove_user(decided).is_err());
        assert!(
            v.auth
                .claim_leftover(decided, &PersonId::new(PERSON))
                .is_err()
        );
    }
    assert_eq!(
        v.auth
            .claim_leftover(&v.machine.user_id, &PersonId::new("nobody"))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[test]
fn herder_claim_opens_the_owner_window_on_a_daemon_with_an_owner() {
    let v = vault();
    // `herder service install` offers the window only while there is no owner.
    assert!(v.auth.open_claim().is_err());
    let (_, creation) = v.auth.sign_in_challenge().unwrap();
    assert!(
        creation.is_none(),
        "no window, nothing to create a passkey from"
    );

    let (code, _) = v.auth.open_recovery().unwrap();
    assert_eq!(code.len(), "XXX-XXX".len());
    let (_, creation) = v.auth.sign_in_challenge().unwrap();
    assert!(creation.is_some(), "the window takes a new owner");

    // A wrong code is refused before any passkey is looked at.
    let refused = v
        .auth
        .sign_in(
            "fp-new-phone",
            "herder-ios/0.1.0",
            herder_protocol::SignIn::ClaimOwnership {
                code: "000-000".into(),
                person: herder_protocol::NewPerson {
                    name: "Tomas".into(),
                    passkey: herder_protocol::PersonPasskey::Register {
                        registration: serde_json::from_value(serde_json::json!({
                            "id": "AA",
                            "rawId": "AA",
                            "response": {"clientDataJSON": "AA", "attestationObject": "AA"},
                            "type": "public-key",
                        }))
                        .unwrap(),
                    },
                },
            },
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Forbidden);
    assert!(refused.message.contains("herder claim"), "{refused:?}");
}
