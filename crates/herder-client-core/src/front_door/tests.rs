use std::sync::Mutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crypto_box::aead::AeadCore;

use super::*;

/// A JWT with `claims`, unsigned: what the device reads of one is never checked.
pub(crate) fn jwt(claims: serde_json::Value) -> String {
    let part = |value: &serde_json::Value| URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.sig",
        part(&serde_json::json!({ "alg": "RS256" })),
        part(&claims)
    )
}

/// The query parameter `key` of `url`.
fn param(url: &str, key: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// A Cloudflare Access edge for one application, and Cloudflare's transfer service, on
/// localhost: `GET /cdn-cgi/access/cli` is the person signing in, after which the transfer
/// service hands the token over, sealed to the key the sign-in named.
struct FakeAccess {
    url: String,
    /// The pages opened, as their paths and queries.
    opened: Arc<Mutex<Vec<String>>>,
}

impl FakeAccess {
    async fn start(app_token: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let signed_in = Arc::new(Mutex::new(None::<String>));
        let (pages, done) = (Arc::clone(&opened), Arc::clone(&signed_in));
        let server = SecretKey::generate(&mut crypto_box::aead::OsRng);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    head.push(byte[0]);
                }
                let raw = String::from_utf8_lossy(&head).into_owned();
                let head = raw.to_ascii_lowercase();
                let target = raw.split_whitespace().nth(1).unwrap_or_default().to_owned();
                let full = format!("http://fake{target}");
                let (status, headers, body) = if head.starts_with("head / ")
                    && head.contains("cf-access-metadata-request: true")
                {
                    let metadata = jwt(serde_json::json!({
                        "aud": "aud1", "hostname": "127.0.0.1", "type": "match",
                    }));
                    (
                        "200 OK",
                        format!("cf-access-metadata: {metadata}\r\n"),
                        String::new(),
                    )
                } else if target.starts_with("/cdn-cgi/access/cli") {
                    pages.lock().unwrap().push(target.clone());
                    *done.lock().unwrap() = param(&full, "token");
                    ("200 OK", String::new(), "signed in".to_owned())
                } else if let Some(key) = target.strip_prefix("/transfer/") {
                    let wanted = done.lock().unwrap().clone();
                    match wanted {
                        Some(wanted) if wanted == key => {
                            let public: [u8; 32] =
                                URL_SAFE.decode(&wanted).unwrap().try_into().unwrap();
                            let nonce = SalsaBox::generate_nonce(&mut crypto_box::aead::OsRng);
                            let answer = serde_json::json!({
                                "app_token": app_token, "org_token": "org",
                            });
                            let sealed = SalsaBox::new(&PublicKey::from(public), &server)
                                .encrypt(&nonce, answer.to_string().as_bytes())
                                .unwrap();
                            let body = STANDARD.encode([nonce.as_slice(), &sealed].concat());
                            let key = URL_SAFE.encode(server.public_key().as_bytes());
                            ("200 OK", format!("service-public-key: {key}\r\n"), body)
                        }
                        _ => ("404 Not Found", String::new(), String::new()),
                    }
                } else {
                    ("404 Not Found", String::new(), String::new())
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        Self { url, opened }
    }
}

#[tokio::test]
async fn a_person_signs_in_in_the_browser_and_the_device_gets_the_token_sealed_to_its_key() {
    let access = FakeAccess::start("app-token-1").await;
    let address = access.url.replace("http://", "ws://");
    let begun = begin(&address, &access.url).await.unwrap();

    // The page: the application's login, naming this sign-in's key and where to go back.
    let page = url::Url::parse(&begun.url).unwrap();
    assert_eq!(page.path(), "/cdn-cgi/access/cli");
    assert_eq!(param(&begun.url, "aud").as_deref(), Some("aud1"));
    assert_eq!(param(&begun.url, "send_org_token").as_deref(), Some("true"));
    assert_eq!(
        param(&begun.url, "edge_token_transfer").as_deref(),
        Some("true")
    );
    let token = param(&begun.url, "token").unwrap();
    assert_eq!(URL_SAFE.decode(&token).unwrap().len(), 32);
    let back = param(&begun.url, "redirect_url").unwrap();
    assert_eq!(param(&back, "token").as_deref(), Some(token.as_str()));

    // Nobody signed in yet: the device waits.
    let waiting = tokio::spawn(async move { begun.token().await });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!waiting.is_finished());

    // The person signs in in the browser.
    get(page.as_str(), &BTreeMap::new(), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let token = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(token, "app-token-1");
    assert_eq!(access.opened.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_token_sealed_to_another_key_does_not_open() {
    let access = FakeAccess::start("app-token-1").await;
    let address = access.url.replace("http://", "ws://");
    let begun = begin(&address, &access.url).await.unwrap();
    let other = begin(&address, &access.url).await.unwrap();
    get(&begun.url, &BTreeMap::new(), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let (_, headers, body) = get(&begun.poll, &BTreeMap::new(), REQUEST_TIMEOUT)
        .await
        .unwrap();
    let sender = &headers["service-public-key"];
    assert_eq!(begun.open(&body, sender).unwrap(), "app-token-1");
    let err = other.open(&body, sender).unwrap_err().to_string();
    assert!(err.contains("does not open"), "{err}");
}

#[tokio::test]
async fn an_address_no_access_application_guards_has_no_front_door() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        }
    });
    let err = format!("{:#}", begin(&address, TRANSFER).await.err().unwrap());
    assert!(
        err.contains("no Cloudflare Access application guards"),
        "{err}"
    );
    let err = format!(
        "{:#}",
        begin("https://hub.example.com", TRANSFER)
            .await
            .err()
            .unwrap()
    );
    assert!(err.contains("not a tunnel's URL"), "{err}");
}

#[test]
fn a_tokens_expiry_is_its_exp() {
    let token = jwt(serde_json::json!({ "exp": 1_900_000_000, "aud": ["a"] }));
    assert_eq!(
        expires_at(&token),
        Some(Timestamp::from_second(1_900_000_000).unwrap())
    );
    assert_eq!(expires_at("not a jwt"), None);
    assert_eq!(expires_at(&jwt(serde_json::json!({ "aud": "a" }))), None);
}

#[test]
fn a_redirect_to_the_login_or_a_refused_token_wants_a_sign_in() {
    let login = Some("https://team.cloudflareaccess.com/cdn-cgi/access/login/hub.example.com");
    let none = tunnel::Sent::new();
    let token = tunnel::Sent::from([(TOKEN_HEADER.to_owned(), "t".to_owned())]);
    let service = tunnel::Sent::from([("cf-access-client-id".to_owned(), "id".to_owned())]);
    assert!(wants_sign_in(302, login, &none));
    assert!(wants_sign_in(403, None, &token));
    assert!(!wants_sign_in(403, None, &none));
    assert!(!wants_sign_in(403, None, &service));
    assert!(!wants_sign_in(
        302,
        Some("https://elsewhere.example.com/login"),
        &none
    ));
    assert!(!wants_sign_in(502, login, &token));
}
