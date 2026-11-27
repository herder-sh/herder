use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::json;

use super::super::fake::{ACCOUNT, FakeCloudflare, Held, TOKEN};
use super::*;

/// A cloudflared that writes the token it got and the arguments it ran with next to itself,
/// then waits to be stopped.
pub(crate) fn fake_cloudflared() -> Result<Vec<u8>> {
    Ok(b"#!/bin/sh\n\
         echo \"$TUNNEL_TOKEN\" > \"$0.token.tmp\" && mv \"$0.token.tmp\" \"$0.token\"\n\
         echo \"$@\" > \"$0.args\"\n\
         exec /bin/sleep 600\n"
        .to_vec())
}

const TUNNEL: &str = "127.0.0.1:7448";

fn wanted(hostname: &str) -> Result<Option<Wanted>, String> {
    Ok(Some(Wanted {
        account_id: ACCOUNT.into(),
        api_token: SecretValue::new(TOKEN).unwrap(),
        hostname: hostname.into(),
    }))
}

fn token() -> Option<SecretValue> {
    SecretValue::new(TOKEN)
}

/// A started tunnel in `dir`, running the fake cloudflared, advertising among `public`'s.
async fn started(
    dir: &Path,
    fake: &FakeCloudflare,
    public: &Arc<PublicAddresses>,
    shutdown: &CancellationToken,
) -> Arc<ManagedTunnel> {
    let tunnel = ManagedTunnel::open(dir, &fake.url, Some(TUNNEL.parse().unwrap())).unwrap();
    tunnel
        .start(Arc::clone(public), fake_cloudflared, shutdown.clone())
        .await;
    tunnel
}

/// Waits until `check` holds.
async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never: {what}");
}

/// Waits until the tunnel's state is `state`, and returns its status.
async fn settled(tunnel: &ManagedTunnel, state: RouteState) -> RouteStatus {
    for _ in 0..400 {
        if let Some(status) = tunnel.status().await
            && status.state == state
        {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never {state:?}: {:?}", tunnel.status().await);
}

fn creates(fake: &FakeCloudflare) -> usize {
    fake.held()
        .calls
        .iter()
        .filter(|call| call.starts_with("POST"))
        .count()
}

/// Reads the file `name` next to the fake cloudflared in `dir`, once written.
async fn written(dir: &Path, name: &str) -> String {
    let path = dir.join(format!("cloudflare/cloudflared.{name}"));
    let mut text = String::new();
    until(&format!("{} written", path.display()), || {
        text = std::fs::read_to_string(&path).unwrap_or_default();
        !text.is_empty()
    })
    .await;
    text.trim().to_owned()
}

#[tokio::test]
async fn the_hub_sets_up_a_tunnel_and_hostname_and_runs_cloudflared_for_them() {
    let fake = FakeCloudflare::start();
    let dir = tempfile::tempdir().unwrap();
    let public = Arc::new(PublicAddresses::new(vec!["home.example.net:7447".into()]));
    let shutdown = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &shutdown).await;
    assert_eq!(tunnel.status().await, None);

    tunnel.want(wanted("hub.example.com"), None);
    let status = settled(&tunnel, RouteState::Up).await;
    assert_eq!(status.url, "wss://hub.example.com");
    assert_eq!(status.error, None);
    {
        let held = fake.held();
        assert_eq!(held.tunnels["tunnel1"]["name"], "herder-hub.example.com");
        assert_eq!(
            held.route("hub.example.com").as_deref(),
            Some("http://127.0.0.1:7448")
        );
        let record = held.records.values().next().unwrap();
        assert_eq!(record["type"], "CNAME");
        assert_eq!(record["proxied"], true);
    }
    // cloudflared gets the token in its environment, never as an argument.
    assert_eq!(
        written(dir.path(), "token").await,
        Held::token_of("tunnel1")
    );
    assert_eq!(
        written(dir.path(), "args").await,
        "tunnel --no-autoupdate run"
    );
    assert_eq!(
        public.get(),
        vec!["home.example.net:7447", "wss://hub.example.com"]
    );

    // Kept for the hub's user only, without the API token, which stays a hub secret.
    let path = dir.path().join("cloudflare/tunnel.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("token-of-tunnel1") && !saved.contains(TOKEN));

    // Told the same again, it makes nothing new.
    let made = creates(&fake);
    tunnel.want(wanted("hub.example.com"), None);
    settled(&tunnel, RouteState::Up).await;
    assert_eq!(creates(&fake), made);
    shutdown.cancel();
}

#[tokio::test]
async fn a_hub_starting_runs_its_tunnel_at_once_and_repairs_what_was_deleted() {
    let fake = FakeCloudflare::start();
    let dir = tempfile::tempdir().unwrap();
    let public = Arc::new(PublicAddresses::default());
    let first = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &first).await;
    tunnel.want(wanted("hub.example.com"), None);
    settled(&tunnel, RouteState::Up).await;
    assert_eq!(
        written(dir.path(), "token").await,
        Held::token_of("tunnel1")
    );
    first.cancel();
    std::fs::remove_file(dir.path().join("cloudflare/cloudflared.token")).unwrap();

    // Someone deletes the DNS record and the tunnel in the dashboard while the hub is down.
    {
        let mut held = fake.held();
        held.records.clear();
        held.tunnels.get_mut("tunnel1").unwrap()["deleted_at"] = json!("2026-11-27T00:00:00Z");
    }

    let shutdown = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &shutdown).await;
    // Before the hub has even heard its settings, cloudflared runs on what was set up.
    assert_eq!(
        written(dir.path(), "token").await,
        Held::token_of("tunnel1")
    );
    std::fs::remove_file(dir.path().join("cloudflare/cloudflared.token")).unwrap();

    tunnel.want(wanted("hub.example.com"), None);
    settled(&tunnel, RouteState::Up).await;
    let route = fake.held().route("hub.example.com");
    assert_eq!(route.as_deref(), Some("http://127.0.0.1:7448"));
    let new = fake
        .held()
        .tunnels
        .values()
        .find(|t| t["deleted_at"].is_null())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new, "tunnel1");
    assert_eq!(written(dir.path(), "token").await, Held::token_of(&new));
    shutdown.cancel();
}

#[tokio::test]
async fn clearing_the_hostname_deletes_the_tunnel_its_record_and_its_state() {
    let fake = FakeCloudflare::start();
    let dir = tempfile::tempdir().unwrap();
    let public = Arc::new(PublicAddresses::default());
    let shutdown = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &shutdown).await;
    tunnel.want(wanted("hub.example.com"), None);
    settled(&tunnel, RouteState::Up).await;

    // Without the token, nothing can be deleted, and the hub says so.
    tunnel.want(Ok(None), None);
    let status = settled(&tunnel, RouteState::Failed).await;
    assert!(status.error.unwrap().contains("API token is gone"));

    tunnel.want(Ok(None), token());
    until("the tunnel is gone", || {
        fake.held().records.is_empty() && !fake.held().tunnels["tunnel1"]["deleted_at"].is_null()
    })
    .await;
    until("the hostname is no longer advertised", || {
        public.get().is_empty()
    })
    .await;
    assert!(fake.held().calls.contains(&format!(
        "DELETE /accounts/{ACCOUNT}/cfd_tunnel/tunnel1/connections"
    )));
    until("the state is gone", || {
        !dir.path().join("cloudflare/tunnel.json").exists()
    })
    .await;
    assert_eq!(tunnel.status().await, None);
    shutdown.cancel();
}

#[tokio::test]
async fn a_new_hostname_replaces_the_old_one() {
    let fake = FakeCloudflare::start();
    let dir = tempfile::tempdir().unwrap();
    let public = Arc::new(PublicAddresses::default());
    let shutdown = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &shutdown).await;
    tunnel.want(wanted("hub.example.com"), None);
    settled(&tunnel, RouteState::Up).await;
    tunnel.want(wanted("fleet.example.com"), None);
    until("the new hostname routes", || {
        fake.held().route("fleet.example.com").is_some()
    })
    .await;
    settled(&tunnel, RouteState::Up).await;
    {
        let held = fake.held();
        assert_eq!(held.route("hub.example.com"), None);
        assert_eq!(held.records.len(), 1);
    }
    until("the new hostname is advertised", || {
        public.get() == vec!["wss://fleet.example.com".to_owned()]
    })
    .await;
    shutdown.cancel();
}

#[tokio::test]
async fn a_record_herder_did_not_make_is_left_alone_but_a_hand_made_tunnels_is_taken_over() {
    let fake = FakeCloudflare::start();
    let dir = tempfile::tempdir().unwrap();
    let public = Arc::new(PublicAddresses::default());
    let shutdown = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &shutdown).await;
    fake.held().records.insert(
        "mine".into(),
        json!({ "id": "mine", "type": "A", "name": "hub.example.com", "content": "203.0.113.9" }),
    );
    tunnel.want(wanted("hub.example.com"), None);
    let status = settled(&tunnel, RouteState::Failed).await;
    assert!(
        status
            .error
            .as_deref()
            .unwrap()
            .contains("already has a A record"),
        "{status:?}"
    );
    assert_eq!(fake.held().records["mine"]["content"], "203.0.113.9");

    fake.held().records.insert(
        "mine".into(),
        json!({ "id": "mine", "type": "CNAME", "name": "hub.example.com",
                "content": "handmade.cfargotunnel.com" }),
    );
    tunnel.want(wanted("hub.example.com"), None);
    settled(&tunnel, RouteState::Up).await;
    assert_eq!(
        fake.held().records["mine"]["content"],
        "tunnel1.cfargotunnel.com"
    );
    shutdown.cancel();
}

#[tokio::test]
async fn a_refused_token_a_zone_outside_the_account_or_no_tunnel_listener_fails_with_why() {
    let fake = FakeCloudflare::start();
    let dir = tempfile::tempdir().unwrap();
    let public = Arc::new(PublicAddresses::default());
    let shutdown = CancellationToken::new();
    let tunnel = started(dir.path(), &fake, &public, &shutdown).await;

    let mut wrong = wanted("hub.example.com").unwrap().unwrap();
    wrong.api_token = SecretValue::new("wrong").unwrap();
    tunnel.want(Ok(Some(wrong)), None);
    let why = settled(&tunnel, RouteState::Failed).await.error.unwrap();
    assert!(why.contains("refused the API token"), "{why}");
    assert!(why.contains("Invalid access token"), "{why}");

    tunnel.want(wanted("hub.example.org"), None);
    let why = settled(&tunnel, RouteState::Failed).await.error.unwrap();
    assert!(why.contains("in no zone"), "{why}");

    tunnel.want(Err("the hub has no secret CF".into()), None);
    let why = settled(&tunnel, RouteState::Failed).await.error.unwrap();
    assert_eq!(why, "the hub has no secret CF");
    assert!(!dir.path().join("cloudflare/tunnel.json").exists());

    let bare = ManagedTunnel::open(dir.path(), &fake.url, None).unwrap();
    bare.start(Arc::clone(&public), fake_cloudflared, shutdown.clone())
        .await;
    bare.want(wanted("hub.example.com"), None);
    let why = settled(&bare, RouteState::Failed).await.error.unwrap();
    assert!(why.contains("set `tunnel`"), "{why}");
    shutdown.cancel();
}

#[test]
fn a_hostname_is_taken_as_written_or_as_a_url() {
    assert_eq!(
        hostname(" https://Hub.Example.com/ ").unwrap(),
        "hub.example.com"
    );
    assert_eq!(
        hostname("wss://hub.example.com").unwrap(),
        "hub.example.com"
    );
    for bad in [
        "hub",
        "-hub.example.com",
        "hub..example.com",
        "hub.example.com:443",
    ] {
        assert!(hostname(bad).is_err(), "{bad}");
    }
}

#[test]
fn cloudflared_reaches_a_listener_on_every_interface_on_loopback() {
    assert_eq!(
        ingress("0.0.0.0:7448".parse().unwrap()),
        "http://127.0.0.1:7448"
    );
    assert_eq!(ingress("[::]:7448".parse().unwrap()), "http://[::1]:7448");
    assert_eq!(
        ingress("10.0.0.4:7448".parse().unwrap()),
        "http://10.0.0.4:7448"
    );
}

#[tokio::test]
async fn cloudflared_that_stops_is_started_again() {
    fn failing() -> Result<Vec<u8>> {
        Ok(b"#!/bin/sh\necho x >> \"$0.runs\"\nexit 1\n".to_vec())
    }
    let dir = tempfile::tempdir().unwrap();
    let (run, watched) = watch::channel(Some(Run {
        token: SecretValue::new("t").unwrap(),
        hostname: "hub.example.com".into(),
    }));
    let status = Status::default();
    let shutdown = CancellationToken::new();
    tokio::spawn(cloudflared::supervise(
        dir.path().to_owned(),
        failing,
        watched,
        Arc::new(PublicAddresses::default()),
        Arc::clone(&status),
        shutdown.clone(),
    ));
    let runs = dir.path().join("cloudflared.runs");
    until("cloudflared ran twice", || {
        std::fs::read_to_string(&runs).is_ok_and(|text| text.lines().count() >= 2)
    })
    .await;
    assert!(
        matches!(&*status.lock().unwrap(), Running::Failed(why) if why.contains("stopped")),
        "{:?}",
        status.lock().unwrap()
    );
    drop(run);
    shutdown.cancel();
}
