use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sentry::protocol::{
    Breadcrumb, Context, DeviceContext, Event, Exception, Frame, Level, Map, Mechanism, OsContext,
    Request, Span, SpanId, Stacktrace, TraceContext, TraceId, Transaction, User, Values,
};
use sentry::test::TestTransport;
use serde_json::json;
use tracing_subscriber::layer::SubscriberExt;

use super::*;
use crate::scrub::{Scrub, is_name, is_word};

/// What would name a person, a machine or their work, as it shows up in real diagnostics.
const SECRETS: &[&str] = &[
    "/Users/tomas/Projects/herder/crates/herder-daemon/src/session/actor.rs",
    "/home/tomas/.local/share/herder/worktrees/herder-1v04xsfm",
    "C:\\Users\\tomas\\herder",
    "10.0.4.17:7447",
    "100.64.0.7",
    "fd7a:115c:a1e0::1",
    "box.tail1234.ts.net",
    "studio.local",
    "wss://box.example.com",
    "tomas@example.com",
    "sk-ant-api03-AbCdEf0123456789xyz",
    "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
    "4bf92f3577b34da6a3ce929d0e0e4736deadbeef",
    "p29-3-sentry-apple",
    "herder-sh/herder",
    "Fix the login bug in the settings page please",
    "Tomas’s MacBook Pro",
];

/// The scrub as a daemon runs it, keeping every trace.
fn scrub() -> Scrub {
    Scrub {
        install_id: "0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10".to_owned(),
        component: Component::Daemon,
        base_rate: 1.0,
    }
}

/// `value` as Sentry receives it, checked for every one of [`SECRETS`].
fn assert_clean(value: &impl serde::Serialize) -> String {
    let json = serde_json::to_string(value).unwrap();
    for secret in SECRETS {
        let escaped = serde_json::to_string(secret).unwrap();
        let bare = escaped.trim_matches('"');
        assert!(!json.contains(bare), "{secret:?} left in {json}");
    }
    json
}

fn frame() -> Frame {
    Frame {
        function: Some("herder_daemon::session::actor::Actor::start".into()),
        module: Some("herder_daemon::session::actor".into()),
        filename: Some("crates/herder-daemon/src/session/actor.rs".into()),
        abs_path: Some(SECRETS[0].into()),
        lineno: Some(1924),
        pre_context: vec!["let prompt = \"Fix the login bug in the settings page please\";".into()],
        context_line: Some(SECRETS[12].into()),
        vars: Map::from([("repo".to_owned(), json!(SECRETS[1]))]),
        in_app: Some(true),
        ..Default::default()
    }
}

fn trace_context(trace_id: TraceId) -> Context {
    Context::Trace(Box::new(TraceContext {
        trace_id,
        span_id: SpanId::default(),
        op: Some("herder_daemon::ws::conn::command".into()),
        description: Some(SECRETS[15].into()),
        data: Map::from([
            ("command".to_owned(), json!("send_prompt")),
            ("route".to_owned(), json!(SECRETS[6])),
            ("events".to_owned(), json!(120)),
        ]),
        ..Default::default()
    }))
}

#[test]
fn an_error_keeps_its_type_and_stack_and_nothing_that_names_anything() {
    let event = Event {
        level: Level::Error,
        message: Some(format!("cannot read {} for {}", SECRETS[0], SECRETS[9])),
        logger: Some("herder_daemon::session::actor".into()),
        culprit: Some(SECRETS[1].into()),
        transaction: Some(SECRETS[13].into()),
        server_name: Some(SECRETS[6].into()),
        user: Some(User {
            id: Some("someone".into()),
            email: Some(SECRETS[9].into()),
            username: Some("tomas".into()),
            ..Default::default()
        }),
        request: Some(Request {
            url: Some(
                "https://box.example.com/api?key=sk-ant-api03-AbCdEf0123456789xyz"
                    .parse()
                    .unwrap(),
            ),
            ..Default::default()
        }),
        breadcrumbs: Values {
            values: vec![Breadcrumb {
                message: Some(SECRETS[15].into()),
                ..Default::default()
            }],
        },
        exception: Values {
            values: vec![Exception {
                ty: "std::io::Error".into(),
                value: Some(format!("No such file or directory: {}", SECRETS[0])),
                module: Some(SECRETS[1].into()),
                stacktrace: Some(Stacktrace {
                    frames: vec![frame()],
                    ..Default::default()
                }),
                mechanism: Some(Mechanism {
                    ty: "panic".into(),
                    description: Some(SECRETS[3].into()),
                    handled: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        },
        tags: Map::from([
            ("host".to_owned(), SECRETS[3].to_owned()),
            ("route".to_owned(), "system_vpn".to_owned()),
        ]),
        extra: Map::from([("token".to_owned(), json!(SECRETS[10]))]),
        contexts: Map::from([
            (
                "device".to_owned(),
                Context::Device(Box::new(DeviceContext {
                    name: Some(SECRETS[16].into()),
                    model: Some("MacBookPro18,3".into()),
                    arch: Some("aarch64".into()),
                    ..Default::default()
                })),
            ),
            (
                "os".to_owned(),
                Context::Os(Box::new(OsContext {
                    name: Some("Linux".into()),
                    version: Some("7.2.15-1-omarchy".into()),
                    kernel_version: Some(SECRETS[7].into()),
                    ..Default::default()
                })),
            ),
            (
                "workspace".to_owned(),
                Context::Other(Map::from([("dir".to_owned(), json!(SECRETS[1]))])),
            ),
        ]),
        ..Default::default()
    };
    let scrubbed = scrub().event(&event);
    let json = assert_clean(&scrubbed);
    assert_eq!(scrubbed.exception.values[0].ty, "std::io::Error");
    assert_eq!(scrubbed.exception.values[0].value, None);
    let frame = &scrubbed.exception.values[0]
        .stacktrace
        .as_ref()
        .unwrap()
        .frames[0];
    assert_eq!(
        frame.function.as_deref(),
        Some("herder_daemon::session::actor::Actor::start")
    );
    assert_eq!(frame.lineno, Some(1924));
    assert_eq!(frame.filename, None);
    assert_eq!(
        scrubbed.message.as_deref(),
        Some("herder_daemon::session::actor")
    );
    assert_eq!(
        scrubbed.user.as_ref().and_then(|user| user.id.as_deref()),
        Some("0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10")
    );
    assert_eq!(
        scrubbed.tags,
        Map::from([("component".to_owned(), "daemon".to_owned())])
    );
    assert!(json.contains("MacBookPro18,3"), "{json}");
    assert!(json.contains("7.2.15-1-omarchy"), "{json}");
    assert!(!json.contains("workspace"), "{json}");
    assert!(scrubbed.breadcrumbs.values.is_empty());
}

#[test]
fn a_trace_keeps_its_spans_names_times_and_words_and_drops_the_rest() {
    let trace_id = TraceId::default();
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let transaction = Transaction {
        name: Some("command".into()),
        start_timestamp: start,
        timestamp: Some(start + Duration::from_millis(80)),
        server_name: Some(SECRETS[6].into()),
        tags: Map::from([("branch".to_owned(), SECRETS[13].to_owned())]),
        extra: Map::from([("prompt".to_owned(), json!(SECRETS[15]))]),
        contexts: Map::from([("trace".to_owned(), trace_context(trace_id))]),
        spans: vec![
            Span {
                trace_id,
                op: Some("herder_client_core::supervisor::connect".into()),
                description: Some("connect".into()),
                start_timestamp: start,
                timestamp: Some(start + Duration::from_millis(40)),
                data: Map::from([
                    ("route".to_owned(), json!("system_vpn")),
                    ("address".to_owned(), json!(SECRETS[3])),
                    ("outcome".to_owned(), json!(SECRETS[8])),
                    ("attempt".to_owned(), json!(2)),
                    ("relayed".to_owned(), json!(false)),
                    ("thread.name".to_owned(), json!("tokio-runtime-worker")),
                ]),
                ..Default::default()
            },
            Span {
                trace_id,
                description: Some(SECRETS[5].into()),
                op: Some(SECRETS[0].into()),
                start_timestamp: start,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let scrubbed = scrub().transaction(&transaction);
    let json = assert_clean(&scrubbed);
    assert_eq!(scrubbed.name.as_deref(), Some("command"));
    let connect = &scrubbed.spans[0];
    assert_eq!(connect.description.as_deref(), Some("connect"));
    assert_eq!(
        connect.data,
        Map::from([
            ("route".to_owned(), json!("system_vpn")),
            ("attempt".to_owned(), json!(2)),
            ("relayed".to_owned(), json!(false)),
        ])
    );
    assert_eq!(scrubbed.spans[1].description, None);
    assert_eq!(scrubbed.spans[1].op, None);
    assert!(json.contains("send_prompt"), "{json}");
    assert!(json.contains("\"events\":120"), "{json}");
}

#[test]
fn only_words_herder_records_pass_for_words() {
    for word in [
        "send_prompt",
        "system_vpn",
        "hub_wireguard",
        "ok",
        "timeout",
        "home_network",
    ] {
        assert!(is_word(word), "{word}");
    }
    for name in ["command", "turn.start", "hub.relay"] {
        assert!(is_name(name), "{name}");
    }
    for text in SECRETS.iter().copied().chain([
        "Send_prompt",
        "",
        "a1b2c3d4e5f6",
        "deadbeefdeadbeefdeadbeef",
        "x y",
    ]) {
        assert!(!is_word(text), "{text:?}");
    }
}

#[test]
fn a_trace_is_picked_alike_everywhere_and_a_slow_one_always_goes() {
    let fast = |trace_id: TraceId, rate: f64| {
        let start = SystemTime::UNIX_EPOCH;
        let transaction = Transaction {
            start_timestamp: start,
            timestamp: Some(start + Duration::from_millis(5)),
            contexts: Map::from([("trace".to_owned(), trace_context(trace_id))]),
            ..Default::default()
        };
        Scrub {
            base_rate: rate,
            ..scrub()
        }
        .keeps(&transaction)
    };
    let low: TraceId = "0000000a77b34da6a3ce929d0e0e4736".parse().unwrap();
    let high: TraceId = "fff92f3577b34da6a3ce929d0e0e4736".parse().unwrap();
    assert!(fast(low, BASE_RATE));
    assert!(!fast(high, BASE_RATE));
    assert!(!fast(low, 0.0));

    let start = SystemTime::UNIX_EPOCH;
    let slow = Transaction {
        start_timestamp: start,
        timestamp: Some(start + SLOW),
        contexts: Map::from([("trace".to_owned(), trace_context(high))]),
        ..Default::default()
    };
    assert!(
        scrub::Scrub {
            base_rate: 0.0,
            ..scrub()
        }
        .keeps(&slow)
    );
}

#[test]
fn a_traceparent_becomes_the_trace_a_span_continues() {
    assert_eq!(
        sentry_trace("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00").as_deref(),
        Some("4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1")
    );
    for bad in [
        "",
        "garbage",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
        "00-/Users/tomas/x-00f067aa0ba902b7-01",
    ] {
        assert_eq!(sentry_trace(bad), None, "{bad:?}");
    }
}

/// The switch is the whole process's: tests that move it take turns.
static SWITCH: Mutex<()> = Mutex::new(());

fn telemetry() -> Telemetry {
    Telemetry {
        install_id: "0b7f1c2e-4d4b-4e0e-9a51-3f6f1b2a9c10".to_owned(),
        sentry_dsn: Some("https://public@sentry.invalid/1".to_owned()),
        posthog_token: None,
        component: Component::Daemon,
    }
}

#[test]
fn nothing_is_sent_while_off_and_everything_stops_once_turned_off() {
    let _turn = SWITCH.lock().unwrap_or_else(PoisonError::into_inner);
    let transport = TestTransport::new();
    let factory: Arc<dyn TransportFactory> = Arc::new(Arc::clone(&transport));
    let subscriber = tracing_subscriber::registry().with(layer());
    tracing::subscriber::with_default(subscriber, || {
        // Off: no client, so nothing is made, let alone sent.
        set(None);
        sentry::capture_message("off", sentry::Level::Error);
        tracing::info_span!("command", command = "send_prompt").in_scope(|| {
            assert_eq!(traceparent(), None);
            tracing::error!("cannot do it");
        });
        assert!(!is_on());

        // Without a DSN, on sends nowhere either.
        set_with_transport(
            Some(Telemetry {
                sentry_dsn: None,
                ..telemetry()
            }),
            Arc::clone(&factory),
            1.0,
        );
        assert!(!is_on());
        sentry::capture_message("no dsn", sentry::Level::Error);
        assert!(transport.fetch_and_clear_envelopes().is_empty());

        // On: a span becomes a trace, scrubbed, and an error an event.
        set_with_transport(Some(telemetry()), Arc::clone(&factory), 1.0);
        assert!(is_on());
        let parent = tracing::info_span!("command", command = "send_prompt", path = SECRETS[0])
            .in_scope(|| {
                tracing::info_span!("replay", events = 3).in_scope(|| {});
                tracing::error!(path = SECRETS[0], "cannot read {}", SECRETS[1]);
                traceparent()
            })
            .expect("a traceparent while on");
        let envelopes = transport.fetch_and_clear_envelopes();
        let json = assert_clean(
            &envelopes
                .iter()
                .map(|e| {
                    let mut bytes = Vec::new();
                    e.to_writer(&mut bytes).unwrap();
                    String::from_utf8(bytes).unwrap()
                })
                .collect::<Vec<_>>(),
        );
        let transaction = envelopes
            .iter()
            .flat_map(|envelope| envelope.items())
            .find_map(|item| match item {
                EnvelopeItemRef::Transaction(transaction) => Some(transaction.clone()),
                _ => None,
            })
            .expect("the command's trace");
        let Some(Context::Trace(trace)) = transaction.contexts.get("trace") else {
            panic!("a trace context");
        };
        assert_eq!(
            parent,
            format!("00-{}-{}-01", trace.trace_id, trace.span_id)
        );
        assert_eq!(transaction.name.as_deref(), Some("command"));
        assert_eq!(transaction.spans[0].description.as_deref(), Some("replay"));
        assert!(json.contains("send_prompt"), "{json}");
        assert!(
            envelopes
                .iter()
                .flat_map(|envelope| envelope.items())
                .any(|item| matches!(item, EnvelopeItemRef::Event(_))),
            "the error, as an event"
        );

        // Off again: what was not sent stays, and nothing new goes.
        let span = tracing::info_span!("command", command = "interrupt");
        set(None);
        drop(span);
        sentry::capture_message("after", sentry::Level::Error);
        tracing::error!("after");
        assert!(!is_on());
        assert!(transport.fetch_and_clear_envelopes().is_empty());
    });
}

use sentry::protocol::EnvelopeItem as EnvelopeItemRef;

#[test]
fn a_variant_is_named_without_its_contents() {
    #[derive(Debug)]
    #[allow(dead_code, reason = "only its Debug is read")]
    enum Body {
        SendPrompt { text: String },
        Interrupt,
        GetLatestEvents(u32),
    }
    let prompt = Body::SendPrompt {
        text: SECRETS[15].to_owned(),
    };
    assert_eq!(variant(&prompt), "send_prompt");
    assert_eq!(variant(&Body::Interrupt), "interrupt");
    assert_eq!(variant(&Body::GetLatestEvents(3)), "get_latest_events");
}
