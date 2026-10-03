//! A demo fleet and session for the screenshots in the PR and docs, rendered from the real
//! window: `cargo test screenshots -- --ignored` with a display (broadway or a headless X
//! server) writes them to `docs/screenshots/p8-4/`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{glib, graphene};
use herder_client_core::{Machine, SessionUpdate};
use herder_protocol::{
    Account, AccountId, ApprovalId, CiStatus, Event, EventBody, Item, ItemBody, ItemId, Mergeable,
    PermissionMode, PrState, Provider, PullRequest, QuestionId, ReviewStatus, Route, SessionHead,
    SessionId, SessionStatus, Timestamp, TurnId, UsageWindow, UserId,
};
use serde_json::json;

use crate::lists::SessionKey;
use crate::lists::tests::{created, head, machine, pr};
use crate::window::MainWindow;

/// Runs the main loop for `time`, so the window lays out and draws.
pub fn settle(time: Duration) {
    let context = glib::MainContext::default();
    let end = Instant::now() + time;
    while Instant::now() < end {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn usage(window: &str, percent: f64) -> UsageWindow {
    UsageWindow {
        window: window.to_owned(),
        used_percent: percent,
        resets_at: None,
    }
}

fn account(id: &str, provider: Provider, label: &str, usage: Vec<UsageWindow>) -> Account {
    Account {
        account_id: AccountId::new(id),
        provider,
        label: label.to_owned(),
        config_dir: None,
        usage,
    }
}

/// `box` with the api session and its task, and a second project.
pub fn machines(status: SessionStatus) -> Vec<Machine> {
    let api = SessionHead {
        status,
        children_need_you: 0,
        ..head("s-api", Some("github.com/acme/app"))
    };
    let tests = SessionHead {
        status: SessionStatus::Running,
        parent: Some(SessionId::new("s-api")),
        task: Some("write the tests".to_owned()),
        ..head("s-tests", Some("github.com/acme/app"))
    };
    let login = SessionHead {
        status: SessionStatus::Idle,
        ..head("s-login", Some("github.com/acme/app"))
    };
    let docs = SessionHead {
        status: SessionStatus::Running,
        ..head("s-docs", Some("github.com/acme/site"))
    };
    let mut fleet = machine("h1", "box", vec![api, tests, login, docs]);
    fleet.accounts = vec![
        account(
            "claude-main",
            Provider::Claude,
            "Claude (work)",
            vec![usage("five_hour", 38.0), usage("seven_day", 12.0)],
        ),
        account(
            "claude-alt",
            Provider::Claude,
            "Claude (personal)",
            vec![usage("five_hour", 4.0)],
        ),
        account(
            "codex-work",
            Provider::Codex,
            "Codex",
            vec![usage("daily", 91.0)],
        ),
    ];
    vec![fleet]
}

/// Events with their times: `ago` seconds before now.
fn timed(bodies: Vec<(i64, Option<&str>, EventBody)>) -> Vec<Event> {
    let now = Timestamp::now().as_second();
    bodies
        .into_iter()
        .zip(1..)
        .map(|((ago, by, body), seq)| Event {
            session_id: SessionId::new("s-api"),
            seq,
            at: Timestamp::from_second(now - ago).unwrap_or(Timestamp::UNIX_EPOCH),
            by: by.map(UserId::new),
            body,
        })
        .collect()
}

fn item(id: &str, turn: &str, body: ItemBody) -> EventBody {
    EventBody::ItemAdded {
        item: Item {
            id: ItemId::new(id),
            turn_id: TurnId::new(turn),
            body,
        },
    }
}

fn result(id: &str, call: &str, output: &str) -> EventBody {
    item(
        id,
        "t1",
        ItemBody::ToolResult {
            call_id: ItemId::new(call),
            output: output.to_owned(),
            is_error: false,
        },
    )
}

/// The first turn of the api session: a health endpoint, as the TUI's screenshots show it.
fn first_turn() -> Vec<(i64, Option<&'static str>, EventBody)> {
    let wt = "/home/dev/.herder/worktrees/api";
    let mut created = created("/home/dev/src/app", "herder/api");
    if let EventBody::SessionCreated {
        worktree, model, ..
    } = &mut created
    {
        *worktree = wt.to_owned();
        *model = "claude-opus-4".to_owned();
    }
    vec![
        (300, None, created),
        (290, None, EventBody::TurnStarted { turn_id: TurnId::new("t1") }),
        (290, Some("dev"), item("u1", "t1", ItemBody::UserMessage {
            text: "Add a health endpoint and test it.".to_owned(),
            attachments: Vec::new(),
        })),
        (288, None, item("r1", "t1", ItemBody::Reasoning {
            text: "Where the router lives: src/api.rs builds it with Router::new.\nA GET /health returning the build version is enough for the load balancer.".to_owned(),
        })),
        (285, None, item("c1", "t1", ItemBody::ToolCall {
            name: "Read".to_owned(),
            input: json!({"file_path": format!("{wt}/src/api.rs")}),
        })),
        (285, None, result("c1r", "c1", "pub fn router() -> Router { … }")),
        (283, None, item("c2", "t1", ItemBody::ToolCall {
            name: "Grep".to_owned(),
            input: json!({"pattern": "Router::new", "path": format!("{wt}/src")}),
        })),
        (283, None, result("c2r", "c2", "Found 3 files\nsrc/api.rs\nsrc/main.rs\nsrc/admin.rs")),
        (270, None, item("c3", "t1", ItemBody::ToolCall {
            name: "Edit".to_owned(),
            input: json!({
                "file_path": format!("{wt}/src/api.rs"),
                "old_string": "pub fn router() -> Router {\n    Router::new()\n        .route(\"/\", get(index))\n}\n",
                "new_string": "pub fn router() -> Router {\n    Router::new()\n        .route(\"/\", get(index))\n        .route(\"/health\", get(health))\n}\n\n/// 200 with the build version, for load balancers.\nasync fn health() -> &'static str {\n    env!(\"CARGO_PKG_VERSION\")\n}\n",
            }),
        })),
        (269, None, result("c3r", "c3", "ok")),
        (240, None, item("c4", "t1", ItemBody::ToolCall {
            name: "Bash".to_owned(),
            input: json!({"command": "cargo test --workspace", "description": "Run the tests"}),
        })),
        (220, None, result("c4r", "c4", "running 12 tests\ntest api::index ... ok\ntest api::health ... ok\ntest admin::login ... ok\ntest admin::logout ... ok\ntest db::migrate ... ok\ntest db::pool ... ok\ntest db::seed ... ok\ntest jobs::queue ... ok\ntest jobs::retry ... ok\ntest jobs::cancel ... ok\ntest web::assets ... ok\ntest web::routes ... ok\n\ntest result: ok. 12 passed; 0 failed")),
        (215, None, item("c5", "t1", ItemBody::ToolCall {
            name: "TodoWrite".to_owned(),
            input: json!({"todos": [
                {"content": "Add the route", "status": "completed"},
                {"content": "Test it", "status": "completed"},
                {"content": "Document it", "status": "in_progress"},
                {"content": "Open a PR", "status": "pending"},
            ]}),
        })),
        (215, None, result("c5r", "c5", "ok")),
        (210, None, EventBody::ChildSpawned {
            child_session_id: SessionId::new("s-tests"),
            task: "write the tests".to_owned(),
        }),
        (200, None, item("a1", "t1", ItemBody::AssistantMessage {
            text: "I added `GET /health`; it returns **200** with the build version so load balancers can probe it.\n\n- one route in `src/api.rs`\n- one test, in a child session".to_owned(),
        })),
        (198, None, EventBody::TurnCompleted { turn_id: TurnId::new("t1") }),
        (150, None, EventBody::PrLinked {
            pr: PullRequest {
                title: "Add a health endpoint".to_owned(),
                head_branch: Some("herder/api".to_owned()),
                review: ReviewStatus::Required,
                ..pr(12, PrState::Open, CiStatus::Passing)
            },
        }),
        (120, None, EventBody::PrLinked {
            pr: PullRequest {
                title: "Document the health endpoint".to_owned(),
                head_branch: Some("herder/api-docs".to_owned()),
                mergeable: Mergeable::Unknown,
                ..pr(9, PrState::Draft, CiStatus::Pending)
            },
        }),
    ]
}

/// The PRs of the demo's other sessions, for the list of every PR.
fn other_prs(session: &str) -> Vec<EventBody> {
    match session {
        "s-login" => vec![EventBody::PrLinked {
            pr: PullRequest {
                title: "Fix the login redirect".to_owned(),
                head_branch: Some("herder/fix-login".to_owned()),
                review: ReviewStatus::Approved,
                ..pr(7, PrState::Merged, CiStatus::Passing)
            },
        }],
        "s-docs" => vec![EventBody::PrLinked {
            pr: PullRequest {
                title: "Rewrite the getting started guide".to_owned(),
                head_branch: Some("herder/docs".to_owned()),
                review: ReviewStatus::ChangesRequested,
                mergeable: Mergeable::Conflicting,
                ..pr(31, PrState::Open, CiStatus::Failing)
            },
        }],
        _ => Vec::new(),
    }
}

/// The api session at one of the moments the screenshots show.
pub fn moment(name: &str) -> (SessionStatus, SessionUpdate) {
    let mut bodies = first_turn();
    let mut streaming = Vec::new();
    let status = match name {
        "chat" => {
            bodies.push((
                40,
                None,
                EventBody::TurnStarted {
                    turn_id: TurnId::new("t2"),
                },
            ));
            bodies.push((
                40,
                Some("dev"),
                item(
                    "u2",
                    "t2",
                    ItemBody::UserMessage {
                        text: "Good. Now document the endpoint in the README.".to_owned(),
                        attachments: Vec::new(),
                    },
                ),
            ));
            streaming.push(Item {
                id: ItemId::new("a2"),
                turn_id: TurnId::new("t2"),
                body: ItemBody::AssistantMessage {
                    text: "Adding a **Health checks** section to `README.md`, after".to_owned(),
                },
            });
            SessionStatus::Running
        }
        "approval" => {
            bodies.push((
                40,
                None,
                EventBody::TurnStarted {
                    turn_id: TurnId::new("t2"),
                },
            ));
            bodies.push((
                40,
                Some("dev"),
                item(
                    "u2",
                    "t2",
                    ItemBody::UserMessage {
                        text: "Good. Now remove the old target directory.".to_owned(),
                        attachments: Vec::new(),
                    },
                ),
            ));
            bodies.push((14, None, item("c6", "t2", ItemBody::ToolCall {
                name: "Bash".to_owned(),
                input: json!({"command": "rm -rf target/", "description": "Remove the old build output"}),
            })));
            bodies.push((
                12,
                None,
                EventBody::ApprovalRequested {
                    approval_id: ApprovalId::new("ap1"),
                    turn_id: TurnId::new("t2"),
                    tool_call_id: ItemId::new("c6"),
                    summary: "Remove the old build output".to_owned(),
                    routed_to: Route::User,
                    reason: None,
                },
            ));
            SessionStatus::NeedsYou
        }
        "question" => {
            bodies.push((
                60,
                None,
                EventBody::TurnStarted {
                    turn_id: TurnId::new("t2"),
                },
            ));
            bodies.push((
                60,
                Some("dev"),
                item(
                    "u2",
                    "t2",
                    ItemBody::UserMessage {
                        text: "Document the endpoint too.".to_owned(),
                        attachments: Vec::new(),
                    },
                ),
            ));
            bodies.push((
                30,
                None,
                EventBody::QuestionAsked {
                    question_id: QuestionId::new("q1"),
                    turn_id: TurnId::new("t2"),
                    text: "Which heading level should the API page use?".to_owned(),
                    choices: vec![
                        "h2 under Reference".to_owned(),
                        "h1, its own page".to_owned(),
                    ],
                    routed_to: Route::User,
                    reason: None,
                },
            ));
            SessionStatus::NeedsYou
        }
        "switched" => {
            bodies.push((
                90,
                Some("dev"),
                EventBody::PermissionModeChanged {
                    mode: PermissionMode::AutoEdit,
                },
            ));
            bodies.push((
                80,
                Some("dev"),
                EventBody::ProviderSwitched {
                    provider: Provider::Codex,
                    account_id: AccountId::new("codex-work"),
                    model: "gpt-5-codex".to_owned(),
                },
            ));
            SessionStatus::Idle
        }
        _ => SessionStatus::Idle,
    };
    (
        status,
        SessionUpdate {
            events: timed(bodies),
            streaming,
        },
    )
}

/// Renders `widget` as it is drawn now to `path`, at twice its size.
pub fn capture(widget: &impl IsA<gtk::Widget>, path: &Path) {
    let widget = widget.as_ref();
    let (width, height) = (widget.width() as f32, widget.height() as f32);
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    // A widget has something to paint once a frame drew it, which a display without a
    // client may hold back: ask for frames until one comes.
    let mut tries = 0;
    let node = loop {
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(2.0, 2.0);
        paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
        if let Some(node) = snapshot.to_node() {
            break node;
        }
        tries += 1;
        assert!(tries < 50, "the widget never draws");
        widget.queue_draw();
        settle(Duration::from_millis(100));
    };
    let renderer = widget
        .native()
        .and_then(|native| native.renderer())
        .expect("the widget is realized");
    let texture = renderer.render_texture(
        node,
        Some(&graphene::Rect::new(0.0, 0.0, width * 2.0, height * 2.0)),
    );
    texture
        .save_to_png(path)
        .expect("the screenshot is written");
}

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/screenshots/p8-4");
    std::fs::create_dir_all(&dir).expect("the screenshot dir");
    dir
}

#[gtk::test]
#[ignore = "writes the PR's screenshots; needs a display"]
fn screenshots() {
    adw::init().expect("libadwaita initializes");
    crate::theme::load();
    let style = adw::StyleManager::default();
    let dir = out_dir();
    // One window for every shot: a display without a client draws new windows late.
    let window = MainWindow::new(None);
    window.present();
    for (scheme, name) in [
        (adw::ColorScheme::ForceDark, "dark"),
        (adw::ColorScheme::ForceLight, "light"),
    ] {
        style.set_color_scheme(scheme);
        for (width, size) in [(1000, "wide"), (400, "narrow")] {
            window.set_size(width, 760);
            for shot in ["chat", "tools", "prs", "list"] {
                let (status, update) = moment(if shot == "tools" { "chat" } else { shot });
                window.show_machines(&[]);
                let machines = machines(status);
                window.show_machines(&machines);
                let key = SessionKey {
                    host_id: machines[0].host_id.clone(),
                    session_id: SessionId::new("s-api"),
                };
                window.apply(&key, &update);
                for head in &machines[0].sessions[1..] {
                    let key = SessionKey {
                        host_id: machines[0].host_id.clone(),
                        session_id: head.session_id.clone(),
                    };
                    let repo = if head.session_id.as_str() == "s-docs" {
                        "/home/dev/src/site"
                    } else {
                        "/home/dev/src/app"
                    };
                    let branch = match head.session_id.as_str() {
                        "s-login" => "herder/fix-login",
                        "s-docs" => "herder/docs",
                        _ => "herder/api-tests",
                    };
                    let mut events = vec![created(repo, branch)];
                    events.extend(other_prs(head.session_id.as_str()));
                    window.apply(
                        &key,
                        &crate::lists::tests::update(head.session_id.as_str(), events),
                    );
                }
                match shot {
                    "list" => {}
                    "prs" => window.pick(1),
                    _ => window.open(&key),
                }
                if shot == "tools" {
                    window.session_view().toggle_tool("Run the tests");
                    window.session_view().toggle_tool("Edit");
                }
                // The composer's editor sizes itself once its lines are laid out, later.
                settle(Duration::from_millis(1200));
                if shot == "tools" {
                    window.session_view().scroll_to_tool("Edit");
                } else {
                    window.scroll_to_end();
                }
                settle(Duration::from_millis(300));
                assert_eq!(window.root().width(), width, "the window takes its size");
                capture(
                    &window.root(),
                    &dir.join(format!("{shot}-{size}-{name}.png")),
                );
                if shot == "chat" {
                    window.session_view().link_pr_for_test();
                    settle(Duration::from_millis(600));
                    capture(&window.root(), &dir.join(format!("link-{size}-{name}.png")));
                    if let Some(dialog) = window.root().visible_dialog() {
                        dialog.force_close();
                    }
                    settle(Duration::from_millis(600));
                }
            }
        }
    }
}
