//! Snapshots of the main screens, drawn on a test backend from fake client-core state.

use herder_client_core::ConnectionState;
use herder_protocol::{ItemBody, ItemId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde_json::json;

use crate::app::Focus;
use crate::app::{App, Effect, Msg};
use crate::fake::{self, added, assistant, item, update};

fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal
}

fn press(app: &mut App, code: KeyCode) {
    app.update(key(code));
}

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// `s2` of [`fake::tree`] mid-turn: a prompt, some tool work, and an answer streaming.
pub(super) fn mid_turn() -> App {
    let mut app = fake::tree();
    let events = vec![
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Add a health endpoint and test it.".into(),
                attachments: Vec::new(),
            },
        ),
        added(
            "i2",
            ItemBody::Reasoning {
                text: "The router lives in src/api.rs.".into(),
            },
        ),
        added(
            "i3",
            ItemBody::ToolCall {
                name: "Bash".into(),
                input: json!({"command": "cargo test --workspace", "timeout": 600}),
            },
        ),
        added(
            "i4",
            ItemBody::ToolResult {
                call_id: ItemId::new("i3"),
                output: "running 12 tests\ntest a ... ok\n\ntest result: ok".into(),
                is_error: false,
            },
        ),
        added(
            "i5",
            assistant(
                "## Plan\nI added `GET /health`; it returns 200 with the build version, \
                 so load balancers can probe it.\n- one route\n- one test\n```rust\nfn health() {}\n```",
            ),
        ),
    ];
    fake::feed(&mut app, "h1", "s2", update("s2", 3, events, Vec::new()));
    let streaming = vec![item("i6", assistant("Now the docs: the endpoint is"))];
    fake::feed(&mut app, "h1", "s2", update("s2", 8, Vec::new(), streaming));
    press(&mut app, KeyCode::Enter);
    // Opening lands in the prompt: leave it for NAVIGATE.
    press(&mut app, KeyCode::Esc);
    app
}

/// The herd of the design spec's mockups: on `box`, `app` with `fix-login` (its turn just
/// ended: done), `api` (running, open mid-turn, PR #12) and its tasks `write tests`
/// (running) and `docs` (asking a question), and `infra`'s `k3s` (idle); on `m2`,
/// `herder`'s `p2d-1-design` (running). `claude-main` has used 38% of its 5h window.
pub(super) fn herd() -> App {
    use herder_protocol::{
        Account, AccountId, CiStatus, EventBody, PrState, Provider, SessionStatus, UsageWindow,
    };
    let project = |name: &str| format!("github.com/acme/{name}");
    let sessions = [
        ("h1", "s1", "app", "herder/fix-login", None, None),
        ("h1", "s2", "app", "herder/api", None, None),
        (
            "h1",
            "s3",
            "app",
            "herder/api-tests",
            Some("s2"),
            Some("write tests"),
        ),
        (
            "h1",
            "s4",
            "app",
            "herder/api-docs",
            Some("s2"),
            Some("docs"),
        ),
        ("h2", "s5", "herder", "herder/p2d-1-design", None, None),
        ("h1", "s6", "infra", "herder/k3s", None, None),
    ];
    let mut machines = vec![
        fake::machine("h1", "box", &[]),
        fake::machine("h2", "m2", &[]),
    ];
    for machine in &mut machines {
        for (host, id, name, ..) in &sessions {
            if machine.host_id.as_str() == *host {
                machine.sessions.push(fake::head(id, Some(&project(name))));
            }
        }
    }
    let window = |name: &str, used_percent| UsageWindow {
        window: name.to_owned(),
        used_percent,
        resets_at: None,
    };
    machines[0].accounts = vec![Account {
        config_dir: None,
        account_id: AccountId::new("claude-main"),
        provider: Provider::Claude,
        label: "claude-main".to_owned(),
        usage: vec![window("five_hour", 38.0), window("seven_day", 12.0)],
    }];
    machines[1].accounts = machines[0].accounts.clone();
    machines[0].resources = Some(fake::host_resources(2));
    machines[0]
        .session_usage
        .insert(herder_protocol::SessionId::new("s2"), fake::session_usage());
    let mut app = App {
        theme: crate::ui::theme::Theme::herder(crate::ui::theme::Mode::Dark),
        // Times the screen shows are counted from a clock that stands still.
        clock: Some(herder_protocol::Timestamp::UNIX_EPOCH),
        ..App::default()
    };
    app.update(Msg::Machines(machines));
    for (host, id, name, branch, parent, task) in sessions {
        let created = fake::created_in(&format!("/home/ann/src/{name}"), branch, parent, task);
        let status = match id {
            "s6" => SessionStatus::Idle,
            _ => SessionStatus::Running,
        };
        let mut bodies = vec![created, fake::status(status)];
        if status == SessionStatus::Running {
            bodies.push(fake::started("turn-1"));
        }
        fake::feed(&mut app, host, id, update(id, 1, bodies, Vec::new()));
    }
    // fix-login's turn ends while another session is in view: done.
    let ended = vec![
        EventBody::TurnCompleted {
            turn_id: herder_protocol::TurnId::new("turn-1"),
            usage: None,
        },
        fake::status(SessionStatus::Idle),
    ];
    fake::feed(&mut app, "h1", "s1", update("s1", 4, ended, Vec::new()));
    let asked = vec![
        fake::question(
            "q1",
            "Which heading level for the API page?",
            &["h2 under Reference", "h1, its own page"],
        ),
        fake::status(SessionStatus::NeedsYou),
    ];
    fake::feed(&mut app, "h1", "s4", update("s4", 4, asked, Vec::new()));
    let mut twelve = fake::pr(12, "Add health endpoint", PrState::Open);
    twelve.ci = CiStatus::Passing;
    fake::feed(
        &mut app,
        "h1",
        "s2",
        update(
            "s2",
            4,
            vec![EventBody::PrLinked { pr: twelve }],
            Vec::new(),
        ),
    );
    let events = vec![
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Add a health endpoint and test it.".into(),
                attachments: Vec::new(),
            },
        ),
        added(
            "i2",
            ItemBody::Reasoning {
                text: "Where the router lives.".into(),
            },
        ),
        added(
            "i3",
            ItemBody::ToolCall {
                name: "Bash".into(),
                input: json!({"command": "cargo test --workspace"}),
            },
        ),
        added(
            "i4",
            ItemBody::ToolResult {
                call_id: ItemId::new("i3"),
                output: "running 12 tests\ntest health::ok ... ok\n\ntest result: ok".into(),
                is_error: false,
            },
        ),
        added(
            "i5",
            assistant(
                "I added `GET /health`; it returns 200 with the build version so load balancers can probe it.",
            ),
        ),
    ];
    fake::feed(&mut app, "h1", "s2", update("s2", 5, events, Vec::new()));
    app.open_key(fake::key("h1", "s2"));
    app
}

#[test]
fn the_shell_reproduces_the_mockups_at_phone_laptop_and_wide_widths() {
    for (width, height) in [(45, 30), (100, 30), (160, 34)] {
        let mut app = herd();
        if width < super::NARROW {
            // A phone gets ASCII only: no symbol a phone font may draw two columns wide.
            let screen = render(&mut app, width, height).backend().to_string();
            assert!(screen.is_ascii(), "{screen}");
        }
        insta::assert_snapshot!(
            format!("shell_{width}x{height}"),
            render(&mut app, width, height).backend()
        );
    }
}

/// [`herd`] with titles: `api` (open) titled automatically and the `docs` task renamed by
/// a user; the other sessions untitled, named by their branch or task.
fn titled_herd() -> App {
    use herder_protocol::{EventBody, TitleSource};
    let mut app = herd();
    let titled = |title: &str, source| EventBody::TitleChanged {
        title: title.to_owned(),
        source,
    };
    let auto = titled("Health endpoint for load balancers", TitleSource::Auto);
    fake::feed(
        &mut app,
        "h1",
        "s2",
        update("s2", 10, vec![auto], Vec::new()),
    );
    let user = titled("API reference page", TitleSource::User);
    fake::feed(
        &mut app,
        "h1",
        "s4",
        update("s4", 6, vec![user], Vec::new()),
    );
    app
}

#[test]
fn titled_and_untitled_sessions_in_the_sidebar_attention_header_and_details() {
    for (width, height) in [(100, 30), (160, 34)] {
        let mut app = titled_herd();
        insta::assert_snapshot!(
            format!("titles_{width}x{height}"),
            render(&mut app, width, height).backend()
        );
    }
    // A phone's switcher lists the attention rows and sessions by title too.
    let mut app = titled_herd();
    app.act(crate::action::Action::GoTo);
    insta::assert_snapshot!("titles_switcher_45x34", render(&mut app, 45, 34).backend());
}

#[test]
fn only_the_details_tell_a_users_title_from_an_automatic_one() {
    let mut app = titled_herd();
    // 100 columns have no details panel; 160 have one.
    let auto = [100, 160].map(|width| render(&mut app, width, 34).backend().to_string());
    // The open session renamed by a user, to the same title.
    let renamed = herder_protocol::EventBody::TitleChanged {
        title: "Health endpoint for load balancers".to_owned(),
        source: herder_protocol::TitleSource::User,
    };
    fake::feed(
        &mut app,
        "h1",
        "s2",
        update("s2", 11, vec![renamed], Vec::new()),
    );
    let user = [100, 160].map(|width| render(&mut app, width, 34).backend().to_string());
    assert_eq!(auto[0], user[0]);
    assert!(auto[1].contains("auto title"), "{}", auto[1]);
    assert!(
        !user[1].contains("auto title") && user[1].contains("renamed"),
        "{}",
        user[1]
    );
}

fn ctrl(c: char) -> Msg {
    Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

#[test]
fn the_phone_switcher_lists_attention_projects_views_and_the_menu() {
    let mut app = herd();
    // The header's switch button, as a tap would press it.
    app.act(crate::action::Action::GoTo);
    insta::assert_snapshot!(render(&mut app, 45, 34).backend());
}

#[test]
fn the_leader_popup_lists_its_keys_over_the_mode_bar() {
    let mut app = herd();
    app.update(ctrl('x'));
    insta::assert_snapshot!(render(&mut app, 100, 30).backend());
}

#[test]
fn the_sidebar_collapses_to_glyphs_and_the_details_lay_over_the_main_pane() {
    let mut app = herd();
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);
    for c in ['b', 'd'] {
        app.update(ctrl('x'));
        press(&mut app, KeyCode::Char(c));
    }
    insta::assert_snapshot!(render(&mut app, 100, 30).backend());
}

#[test]
fn a_pending_approval_puts_the_shell_in_approval_mode() {
    for (width, height) in [(45, 30), (100, 30)] {
        let mut app = herd();
        fake::feed(
            &mut app,
            "h1",
            "s2",
            update(
                "s2",
                20,
                vec![fake::approval("a1", "rm -rf target/")],
                Vec::new(),
            ),
        );
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode(), Some(crate::nav::Mode::Approval));
        insta::assert_snapshot!(
            format!("approval_{width}x{height}"),
            render(&mut app, width, height).backend()
        );
    }
}

#[test]
fn no_machines_shows_how_to_pair() {
    let mut app = App::default();
    insta::assert_snapshot!(render(&mut app, 80, 16).backend());
}

#[test]
fn sessions_show_status_and_the_task_tree() {
    let mut app = fake::tree();
    let terminal = render(&mut app, 80, 12);
    insta::assert_snapshot!(terminal.backend());
    // A state is a glyph in its colour: "needs you" stands out.
    let buffer = terminal.backend().buffer();
    let row = |y: u16| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    let y = (0..12).find(|&y| row(y).contains("◉ app · api")).unwrap();
    let x = (0..80).find(|&x| buffer[(x, y)].symbol() == "◉").unwrap();
    assert_eq!(buffer[(x, y)].fg, Color::Magenta);
}

#[test]
fn a_session_streams_its_transcript() {
    let mut app = mid_turn();
    insta::assert_snapshot!(render(&mut app, 100, 30).backend());
}

#[test]
fn a_long_transcript_shows_its_end() {
    let mut app = mid_turn();
    insta::assert_snapshot!(render(&mut app, 100, 12).backend());
    // Scrolled to the top, it shows its start.
    press(&mut app, KeyCode::Char('g'));
    let terminal = render(&mut app, 100, 12);
    assert!(
        terminal
            .backend()
            .to_string()
            .contains("Add a health endpoint"),
        "{}",
        terminal.backend()
    );
}

#[test]
fn the_status_line_shows_each_connection() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    let mut down = fake::machine("h2", "laptop", &[]);
    down.connection = ConnectionState::Disconnected {
        error: "connection refused".into(),
    };
    let mut up = fake::machine("h3", "ci", &[]);
    up.connection = ConnectionState::Connecting;
    machines.extend([down, up]);
    app.update(Msg::Machines(machines));
    insta::assert_snapshot!(render(&mut app, 100, 10).backend());
}

#[test]
fn a_vault_groups_sessions_by_host_and_marks_offline_ones() {
    let mut app = fake::vault();
    insta::assert_snapshot!(render(&mut app, 120, 10).backend());
    // By project, each session names its host, and an offline one says so.
    press(&mut app, KeyCode::Char('v'));
    insta::assert_snapshot!(render(&mut app, 120, 10).backend());
}

#[test]
fn an_offline_hosts_session_is_read_only_and_offers_the_fork() {
    for (width, height) in [(120, 24), (45, 30)] {
        let mut app = fake::vault();
        app.choose_row(crate::app::Row::Session {
            key: fake::key("v", "s2"),
            depth: 0,
        });
        if width < super::NARROW {
            // The list says since when the host is offline, and its bar offers to fork the
            // selected session.
            let screen = render(&mut app, width, height).backend().to_string();
            // In ASCII, as on a phone.
            assert!(screen.contains("offline - 2h 5m ago"), "{screen}");
            assert!(screen.contains("F fork"), "{screen}");
        }
        press(&mut app, KeyCode::Enter);
        insta::assert_snapshot!(
            format!("offline_session_{width}x{height}"),
            render(&mut app, width, height).backend()
        );
    }
}

#[test]
fn the_fork_dialog_at_three_widths() {
    let mut app = fake::vault();
    app.choose_row(crate::app::Row::Session {
        key: fake::key("v", "s2"),
        depth: 0,
    });
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('F'));
    at_three_widths("fork", &mut app);
    // Paired with devbox as an owner: enter forks it there.
    let mut machines = app.machines.clone();
    machines.push(fake::machine("devbox", "devbox", &[]));
    app.update(Msg::Machines(machines));
    insta::assert_snapshot!("fork_paired", render(&mut app, 100, 24).backend());
}

#[test]
fn the_terminals_list_at_three_widths() {
    let mut app = crate::terminal::app_tests::with_terminals();
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('j'));
    at_three_widths("terminals", &mut app);
}

#[test]
fn the_sessions_prs_tab_at_three_widths() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('j'));
    at_three_widths("prs_tab", &mut app);
}

#[test]
fn the_accounts_and_fleet_views_show_their_keys_in_the_mode_bar() {
    let mut app = with_accounts();
    press(&mut app, KeyCode::Char('A'));
    let screen = render(&mut app, 100, 30).backend().to_string();
    assert!(
        screen.contains("n add account  l log in again  r reconnect  esc back"),
        "{screen}"
    );
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('m'));
    let screen = render(&mut app, 100, 30).backend().to_string();
    assert!(screen.contains("a add machine  n add account"), "{screen}");
}

#[test]
fn a_moved_session_says_where_it_went_and_is_read_only() {
    let mut app = fake::moved();
    app.choose_row(crate::app::Row::Session {
        key: fake::key("laptop", "s2"),
        depth: 0,
    });
    press(&mut app, KeyCode::Enter);
    insta::assert_snapshot!(render(&mut app, 120, 12).backend());
}

#[test]
fn help_lists_the_keys() {
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char('?'));
    insta::assert_snapshot!(render(&mut app, 80, 36).backend());
}

/// `s2` of [`fake::tree`] open, with `bodies` fed to it from seq 3.
fn open_s2(bodies: Vec<herder_protocol::EventBody>) -> App {
    let mut app = fake::tree();
    fake::feed(&mut app, "h1", "s2", update("s2", 3, bodies, Vec::new()));
    press(&mut app, KeyCode::Enter);
    app
}

#[test]
fn an_approval_is_prompted_and_badged() {
    let mut app = open_s2(vec![
        fake::started("turn-1"),
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Clean the build.".into(),
                attachments: Vec::new(),
            },
        ),
        fake::approval("a1", "Bash: rm -rf target"),
        fake::approval("a2", "Bash: cargo build"),
    ]);
    let terminal = render(&mut app, 100, 20);
    insta::assert_snapshot!(terminal.backend());
    let screen = terminal.backend().to_string();
    assert!(screen.contains("APPROVAL"), "{screen}");
}

#[test]
fn a_prompts_images_show_under_it() {
    let mut app = open_s2(vec![
        fake::started("turn-1"),
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Match the mockup.".into(),
                attachments: vec![
                    herder_protocol::Attachment {
                        attachment_id: herder_protocol::AttachmentId::new("01J9A"),
                        media_type: "image/png".into(),
                        size: 12 * 1024 + 7,
                    },
                    herder_protocol::Attachment {
                        attachment_id: herder_protocol::AttachmentId::new("01J9B"),
                        media_type: "image/jpeg".into(),
                        size: 3 * 1024 * 1024 / 2,
                    },
                ],
            },
        ),
    ]);
    let terminal = render(&mut app, 100, 20);
    let screen = terminal.backend().to_string();
    assert!(screen.contains("image 1 · 12 KB"), "{screen}");
    assert!(screen.contains("image 2 · 1.5 MB"), "{screen}");
    insta::assert_snapshot!(terminal.backend());
    // The status bar offers to open or save them.
    app.focus = Focus::Transcript;
    let screen = render(&mut app, 160, 20).backend().to_string();
    assert!(screen.contains("o open images"), "{screen}");
}

#[test]
fn the_prompt_shows_its_images_as_chips() {
    let mut app = open_s2(vec![]);
    app.focus = Focus::Composer;
    let image = |bytes: usize| herder_protocol::Image {
        media_type: "image/png".into(),
        data: herder_protocol::Bytes(vec![0; bytes]),
    };
    app.update(Msg::Attached {
        word: None,
        result: Ok(image(340 * 1024)),
    });
    app.update(Msg::Attached {
        word: None,
        result: Ok(image(1_258_291)),
    });
    // A third still loading.
    app.compose.loading = 1;
    fake::type_text(&mut app, "why does the sidebar overlap here?");
    for width in [45, 100] {
        let terminal = render(&mut app, width, 20);
        let screen = terminal.backend().to_string();
        assert!(screen.contains("340 KB"), "{screen}");
        insta::assert_snapshot!(format!("prompt_chips_{width}"), terminal.backend());
    }
}

#[test]
fn a_question_lists_its_choices() {
    let mut app = open_s2(vec![
        fake::started("turn-1"),
        fake::question(
            "q1",
            "Which database should the cache use?",
            &["SQLite", "Postgres"],
        ),
    ]);
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn a_prompt_sent_during_a_turn_shows_queued() {
    let mut app = open_s2(vec![fake::started("turn-1")]);
    let streaming = vec![item("i6", assistant("Working on it"))];
    fake::feed(&mut app, "h1", "s2", update("s2", 4, Vec::new(), streaming));
    press(&mut app, KeyCode::Char('i'));
    fake::type_text(&mut app, "then add docs");
    press(&mut app, KeyCode::Enter);
    fake::type_text(&mut app, "and a changelog entry");
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn the_palette_shows_why_a_command_failed() {
    let mut app = open_s2(vec![]);
    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "mode yolo");
    press(&mut app, KeyCode::Enter);
    insta::assert_snapshot!(render(&mut app, 100, 12).backend());
}

#[test]
fn a_refused_command_shows_in_the_session_view() {
    let mut app = open_s2(vec![]);
    app.update(Msg::Sent {
        origin: crate::compose::Origin::Session(fake::key("h1", "s2")),
        result: Err("claude cannot switch models mid-session".into()),
    });
    insta::assert_snapshot!(render(&mut app, 100, 12).backend());
}

#[test]
fn the_new_session_dialog() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    machines[0].accounts = vec![fake::account("claude-main", "Main")];
    app.update(Msg::Machines(machines));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Tab);
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn the_session_list_badges_each_sessions_prs() {
    let mut app = fake::with_prs();
    // A sidebar of 30 columns or more has room for the badges.
    app.layout.sidebar = 34;
    let terminal = render(&mut app, 80, 10);
    insta::assert_snapshot!(terminal.backend());
    // The badge's number takes its PR state's colour, its marks the checks'.
    let buffer = terminal.backend().buffer();
    let row = |y: u16| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    let s2 = (0..10).find(|&y| row(y).contains("#7")).unwrap();
    let at = row(s2).find("#7").unwrap();
    let x = u16::try_from(row(s2)[..at].chars().count()).unwrap();
    assert_eq!(buffer[(x, s2)].fg, Color::Green);
    assert_eq!(buffer[(x + 3, s2)].symbol(), "✓");
}

#[test]
fn p_shows_the_open_sessions_prs_tab() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('j'));
    insta::assert_snapshot!(render(&mut app, 110, 24).backend());
    // Back in the chat, the tab row counts them.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Transcript);
    let terminal = render(&mut app, 110, 24);
    let screen = terminal.backend().to_string();
    assert!(screen.contains("prs 2"), "{screen}");
    assert!(!screen.contains("x unlink"), "{screen}");
}

#[test]
fn a_session_without_prs_has_no_strip() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 110, 14).backend().to_string();
    assert!(!screen.contains("pull requests"), "{screen}");
}

#[test]
fn every_sessions_prs_are_listed_under_their_session() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Char('G'));
    insta::assert_snapshot!(render(&mut app, 110, 14).backend());
}

#[test]
fn no_prs_anywhere_says_so() {
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char('P'));
    let screen = render(&mut app, 110, 10).backend().to_string();
    assert!(screen.contains("No pull requests are linked"), "{screen}");
}

#[test]
fn the_link_prompt_names_its_session() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('L'));
    for c in "https://github.com/acme/app/pull/31".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    insta::assert_snapshot!(render(&mut app, 90, 12).backend());
}

#[test]
fn a_notice_replaces_the_connections_until_the_next_key() {
    let mut app = fake::tree();
    app.update(Msg::Notice(
        "pull request #4 does not exist in acme/app".into(),
    ));
    let screen = render(&mut app, 90, 8).backend().to_string();
    assert!(screen.contains("#4 does not exist"), "{screen}");
    press(&mut app, KeyCode::Char('j'));
    let screen = render(&mut app, 90, 8).backend().to_string();
    assert!(!screen.contains("#4 does not exist"), "{screen}");
    assert!(screen.contains("● box"), "{screen}");
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

pub(super) const LINK: &str = "herder://pair?host=192.168.1.5%3A7447&host=10.0.0.2%3A7447\
                    &fp=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\
                    &code=ABCDE-FGHJK";

#[test]
fn the_machines_panel_shows_connections_and_fingerprints() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    let mut down = fake::machine("h2", "laptop", &[]);
    down.connection = ConnectionState::Disconnected {
        error: "connection refused".into(),
    };
    down.role = None;
    machines.push(down);
    app.update(Msg::Machines(machines));
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('j'));
    insta::assert_snapshot!(render(&mut app, 90, 20).backend());
}

#[test]
fn a_vaults_disk_and_each_hosts_usage_with_a_warning_when_nearly_full() {
    let text = |app: &mut App, width| format!("{:?}", render(app, width, 30).backend());
    let mut app = fake::vault_storage(212.4);
    let shown = text(&mut app, 100);
    assert!(shown.contains("212.4 / 480.0 GiB, 44%"), "{shown}");
    assert!(!shown.contains("nearly full"), "{shown}");
    let mut app = fake::vault_storage(412.8);
    at_three_widths("vault_storage", &mut app);
    let shown = text(&mut app, 100);
    assert!(shown.contains("3 hosts"), "{shown}");
    assert!(shown.contains("disk 86%"), "{shown}");
    assert!(
        shown.contains("412.8 / 480.0 GiB, 86% · nearly full"),
        "{shown}"
    );
    assert!(
        shown.contains("devbox    4 sessions · images 340 MiB of 1.0 GiB"),
        "{shown}"
    );
    assert!(
        shown.contains("laptop    1 session  · images off"),
        "{shown}"
    );
}

#[test]
fn the_machines_panel_shows_connection_quality() {
    let mut app = fake::tree();
    app.clock = Some(herder_protocol::Timestamp::from_second(320).unwrap());
    let mut machines = app.machines.clone();
    machines[0].quality = herder_client_core::ConnectionQuality {
        connected_since: Some(herder_protocol::Timestamp::from_second(200).unwrap()),
        reconnects: 1,
        last_rtt_ms: Some(12),
        average_rtt_ms: Some(14),
        min_rtt_ms: Some(9),
        max_rtt_ms: Some(31),
        missed_pongs: 2,
    };
    app.update(Msg::Machines(machines));
    press(&mut app, KeyCode::Char('m'));
    insta::assert_snapshot!(render(&mut app, 90, 20).backend());
}

#[test]
fn the_add_account_dialog_picks_a_provider_and_names_the_account() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    machines[0].accounts = vec![herder_protocol::Account {
        config_dir: None,
        account_id: herder_protocol::AccountId::new("claude-main"),
        provider: herder_protocol::Provider::Claude,
        label: "Main".into(),
        usage: Vec::new(),
    }];
    app.update(Msg::Machines(machines));
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Tab);
    typed(&mut app, "codex-2");
    insta::assert_snapshot!(render(&mut app, 90, 20).backend());
}

#[test]
fn the_add_dialog_takes_a_link_or_its_fields() {
    let mut app = App::default();
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Tab);
    typed(&mut app, "box.lan");
    press(&mut app, KeyCode::Tab);
    typed(&mut app, "9f86d0");
    press(&mut app, KeyCode::Enter);
    insta::assert_snapshot!(render(&mut app, 90, 20).backend());
}

#[test]
fn the_add_dialog_shows_the_fingerprint_to_check() {
    let mut app = fake::tree();
    app.update(Msg::Paste(LINK.into()));
    insta::assert_snapshot!(render(&mut app, 90, 16).backend());
    press(&mut app, KeyCode::Enter);
    let mut paired = fake::machine("h2", "laptop", &[]);
    paired.fingerprint = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into();
    app.update(Msg::Paired(Ok(Box::new(paired))));
    insta::assert_snapshot!("paired", render(&mut app, 90, 16).backend());
}

#[test]
fn a_primary_counts_its_children_and_badges_the_ones_waiting_on_you() {
    let mut app = fake::escalated();
    let terminal = render(&mut app, 80, 10);
    // The primary's row counts the task that waits on the user.
    let buffer = terminal.backend().buffer();
    let sidebar = |y: u16| (0..25).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    let y = (0..10).find(|&y| sidebar(y).contains("app · api")).unwrap();
    assert!(
        sidebar(y).trim_end().ends_with('1'),
        "{}",
        terminal.backend()
    );
    // Folded, the row says how many it hides, and the count stays.
    press(&mut app, KeyCode::Char('z'));
    insta::assert_snapshot!(render(&mut app, 80, 10).backend());
    let screen = render(&mut app, 80, 10).backend().to_string();
    assert!(screen.contains("▸2 1"), "{screen}");
}

#[test]
fn the_inbox_shows_each_request_with_its_task_reason_and_note() {
    let mut app = fake::escalated();
    app.clock = Some(herder_protocol::Timestamp::from_second(320).unwrap());
    press(&mut app, KeyCode::Char('i'));
    insta::assert_snapshot!(render(&mut app, 110, 20).backend());
    press(&mut app, KeyCode::Enter);
    fake::type_text(&mut app, "9000");
    insta::assert_snapshot!("inbox_answer", render(&mut app, 110, 20).backend());
}

#[test]
fn an_empty_inbox_says_so() {
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char('i'));
    let screen = render(&mut app, 100, 10).backend().to_string();
    assert!(screen.contains("Nothing is waiting on you."), "{screen}");
}

#[test]
fn a_child_names_its_primary_and_what_the_primary_answered() {
    let mut app = fake::escalated();
    let resolved = herder_protocol::EventBody::ApprovalResolved {
        approval_id: herder_protocol::ApprovalId::new("a7"),
        decision: herder_protocol::ApprovalOutcome::Allow,
        answered_by: herder_protocol::Answerer::Primary {
            session_id: herder_protocol::SessionId::new("s2"),
        },
    };
    fake::feed(
        &mut app,
        "h1",
        "s3",
        update("s3", 7, vec![resolved], Vec::new()),
    );
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    insta::assert_snapshot!(render(&mut app, 110, 20).backend());
}

/// Snapshots `app` on a phone-sized screen and a desktop one, named `name_45x40` and
/// `name_120x40`.
fn narrow_and_wide(name: &str, app: &mut App) {
    for (width, height) in [(45, 40), (120, 40)] {
        let terminal = render(app, width, height);
        assert_last_column_blank(terminal.backend().buffer());
        insta::assert_snapshot!(format!("{name}_{width}x{height}"), terminal.backend());
    }
}

/// Snapshots `app` at the screenshots' sizes, a phone, a laptop terminal and a wide one,
/// named `name_45x40`, `name_100x30` and `name_160x40`.
fn at_three_widths(name: &str, app: &mut App) {
    for (width, height) in [(45, 40), (100, 30), (160, 40)] {
        let terminal = render(app, width, height);
        assert_last_column_blank(terminal.backend().buffer());
        insta::assert_snapshot!(format!("{name}_{width}x{height}"), terminal.backend());
    }
}

/// Nothing is written in the last column, where terminals disagree about wrapping.
fn assert_last_column_blank(buffer: &ratatui::buffer::Buffer) {
    let x = buffer.area.right() - 1;
    for y in buffer.area.top()..buffer.area.bottom() {
        assert_eq!(buffer[(x, y)], ratatui::buffer::Cell::default(), "row {y}");
    }
}

#[test]
fn up_to_64_columns_is_a_phone() {
    let mut app = fake::tree();
    // One pane: the list, full width, with a bar of buttons over the status line.
    for (width, phone) in [(64, true), (65, false)] {
        let screen = render(&mut app, width, 20).backend().to_string();
        assert_eq!(screen.contains("> open"), phone, "{width}:\n{screen}");
    }
}

#[test]
fn a_narrow_screen_shows_ascii_unless_glyphs_chose_unicode() {
    let mut app = fake::tree();
    let text = "Done — the “fix” works… mostly → ship it";
    let events = vec![added("i1", assistant(text))];
    fake::feed(&mut app, "h1", "s2", update("s2", 3, events, Vec::new()));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    let ascii = render(&mut app, 45, 20).backend().to_string();
    // Not even box drawing: it is two columns wide on some phones.
    assert!(ascii.is_ascii(), "{ascii}");
    assert!(
        ascii.contains(r#"Done - the "fix" works. mostly > ship"#),
        "{ascii}"
    );
    insta::assert_snapshot!(render(&mut app, 45, 20).backend());

    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "glyphs emoji");
    assert_eq!(app.update(key(KeyCode::Enter)), []);
    let palette = app.compose.palette.as_ref().unwrap();
    assert_eq!(
        palette.error.as_deref(),
        Some("usage: glyphs ascii|unicode")
    );
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char(':'));
    fake::type_text(&mut app, "glyphs unicode");
    assert_eq!(app.update(key(KeyCode::Enter)), [Effect::Save]);
    let unicode = render(&mut app, 45, 20).backend().to_string();
    assert!(
        unicode.contains("Done — the “fix” works… mostly → ship"),
        "{unicode}"
    );
}

#[test]
fn the_session_list_on_narrow_and_wide_screens() {
    let mut app = fake::with_prs();
    fake::feed(
        &mut app,
        "h1",
        "s3",
        update(
            "s3",
            5,
            vec![fake::approval("a1", "Bash: rm -rf target")],
            Vec::new(),
        ),
    );
    narrow_and_wide("list", &mut app);
}

#[test]
fn a_session_on_narrow_and_wide_screens() {
    let mut app = mid_turn();
    narrow_and_wide("session", &mut app);
}

#[test]
fn an_approval_on_a_narrow_screen_answers_without_esc() {
    let mut app = open_s2(vec![
        fake::started("turn-1"),
        fake::approval("a1", "Bash: rm -rf target"),
    ]);
    press(&mut app, KeyCode::Char('i'));
    insta::assert_snapshot!(render(&mut app, 45, 40).backend());
}

/// The P12.4 "Done when": every session's work state, attention first, without the archived
/// and moved ones.
#[test]
fn the_board_on_narrow_and_wide_screens() {
    let mut app = fake::board();
    press(&mut app, KeyCode::Char('B'));
    press(&mut app, KeyCode::Char('j'));
    at_three_widths("board", &mut app);
}

#[test]
fn the_inbox_on_narrow_and_wide_screens() {
    let mut app = fake::escalated();
    // Two minutes after the escalation.
    app.clock = Some(herder_protocol::Timestamp::from_second(320).unwrap());
    press(&mut app, KeyCode::Char('i'));
    at_three_widths("inbox", &mut app);
    // Answering: the answer is typed in a prompt under the list.
    press(&mut app, KeyCode::Enter);
    fake::type_text(&mut app, "9000");
    at_three_widths("inbox_answer", &mut app);
}

#[test]
fn every_pr_on_narrow_and_wide_screens() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('P'));
    at_three_widths("prs", &mut app);
}

#[test]
fn the_help_scrolls_on_a_small_screen() {
    let mut app = fake::tree();
    press(&mut app, KeyCode::Char('?'));
    let top = render(&mut app, 45, 30).backend().to_string();
    assert!(top.contains("j/k scroll"), "{top}");
    for _ in 0..100 {
        press(&mut app, KeyCode::Char('j'));
    }
    let end = render(&mut app, 45, 30).backend().to_string();
    assert!(app.help);
    assert!(end.contains("show or hide"), "{end}");
    assert!(!end.contains("open the selected session"), "{end}");
    // Any other key closes it.
    press(&mut app, KeyCode::Char('x'));
    assert!(!app.help);
}

/// The screen `app` draws on a fresh `width` by `height` terminal.
fn fresh(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    render(app, width, height).backend().buffer().clone()
}

/// Resizes the test terminal as a terminal would and lets the app repaint, as the event loop
/// does.
fn resize(terminal: &mut Terminal<TestBackend>, app: &mut App, width: u16, height: u16) {
    terminal.backend_mut().resize(width, height);
    let effects = app.update(Msg::Resize);
    assert_eq!(effects, [crate::app::Effect::Repaint]);
    super::paint(
        terminal,
        app,
        super::Paint::Full,
        &mut ratatui::buffer::Buffer::default(),
    )
    .unwrap();
}

#[test]
fn resizing_narrow_wide_narrow_leaves_no_stale_cells() {
    let mut app = mid_turn();
    let mut terminal = Terminal::new(TestBackend::new(45, 40)).unwrap();
    super::paint(
        &mut terminal,
        &mut app,
        super::Paint::Diff,
        &mut ratatui::buffer::Buffer::default(),
    )
    .unwrap();
    for (width, height) in [(120, 40), (45, 40), (45, 22), (45, 40)] {
        resize(&mut terminal, &mut app, width, height);
        assert_eq!(
            *terminal.backend().buffer(),
            fresh(&mut app, width, height),
            "after resizing to {width}x{height}"
        );
    }
}

#[test]
fn a_resize_back_to_the_same_size_repaints_what_the_terminal_reflowed() {
    use ratatui::backend::Backend;
    use ratatui::buffer::Cell;

    let mut app = fake::tree();
    let mut terminal = Terminal::new(TestBackend::new(45, 40)).unwrap();
    let mut last = ratatui::buffer::Buffer::default();
    super::paint(&mut terminal, &mut app, super::Paint::Diff, &mut last).unwrap();
    // A phone keyboard opened and closed between two draws: the terminal reflowed its rows,
    // and its size is the one ratatui last drew at.
    let mut junk = Cell::default();
    junk.set_symbol("#");
    let cells: Vec<(u16, u16, Cell)> = (0..40)
        .flat_map(|y| (0..45).map(move |x| (x, y)))
        .map(|(x, y)| (x, y, junk.clone()))
        .collect();
    terminal
        .backend_mut()
        .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
        .unwrap();
    resize(&mut terminal, &mut app, 45, 40);
    assert_eq!(*terminal.backend().buffer(), fresh(&mut app, 45, 40));
}

/// [`fake::tree`] whose machine has three accounts, two with usage, and pins its sessions,
/// beside a disconnected machine with none; `s2` and `s3` run on `claude-main`.
fn with_accounts() -> App {
    let mut app = fake::tree();
    add_accounts(&mut app);
    app
}

/// Gives `app`'s first machine three accounts with usage, Claude's and Codex's, and adds a
/// disconnected `laptop`.
pub(super) fn add_accounts(app: &mut App) {
    use herder_protocol::{Provider, Timestamp, UsageWindow};

    let now = app.now().as_second();
    // Half a minute past each reset time, so the countdown reads the same while the test runs.
    let window = |name: &str, used_percent: f64, secs: i64| UsageWindow {
        window: name.into(),
        used_percent,
        resets_at: Some(Timestamp::from_second(now + secs + 30).unwrap()),
    };
    let mut machines = app.machines.clone();
    let mut main = fake::account("claude-main", "Main");
    main.usage = vec![
        window("five_hour", 42.0, 2 * 3600 + 13 * 60),
        window("seven_day", 74.0, 5 * 86400 + 3 * 3600),
        window("seven_day_fable", 93.0, 5 * 86400 + 3 * 3600),
    ];
    let mut codex = fake::account("codex", "codex");
    codex.provider = Provider::Codex;
    codex.usage = vec![
        window("five_hour", 8.0, 40 * 60),
        window("weekly", 20.0, 86400),
    ];
    let work = fake::account("claude-work", "Work");
    machines[0].accounts = vec![main, work, codex];
    machines[0].failover = herder_protocol::FailoverSettings { pin: true };
    let mut laptop = fake::machine("h2", "laptop", &[]);
    laptop.connection = ConnectionState::Disconnected {
        error: "connection refused".into(),
    };
    machines.push(laptop);
    app.update(Msg::Machines(machines));
}

#[test]
fn the_accounts_screen_on_narrow_and_wide_screens() {
    let mut app = with_accounts();
    press(&mut app, KeyCode::Char('A'));
    at_three_widths("accounts", &mut app);
}

#[test]
fn the_add_account_dialog_over_the_accounts_screen() {
    let mut app = with_accounts();
    press(&mut app, KeyCode::Char('A'));
    press(&mut app, KeyCode::Char('n'));
    insta::assert_snapshot!(render(&mut app, 45, 40).backend());
}

#[test]
fn the_switch_dialog_on_narrow_and_wide_screens() {
    let mut app = with_accounts();
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    narrow_and_wide("switch", &mut app);
}

#[test]
fn projects_across_machines_on_narrow_and_wide_screens() {
    let mut app = fake::projects();
    narrow_and_wide("projects", &mut app);
}

#[test]
fn each_projects_prs_on_narrow_and_wide_screens() {
    let mut app = fake::projects();
    press(&mut app, KeyCode::Char('P'));
    at_three_widths("project_prs", &mut app);
}

#[test]
fn a_new_session_from_a_project_on_narrow_and_wide_screens() {
    let mut app = fake::projects();
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('n'));
    narrow_and_wide("project_new_session", &mut app);
}

#[test]
fn host_resources_in_the_machines_panel_on_narrow_and_wide_screens() {
    let mut app = fake::tree();
    fake::with_resources(&mut app, fake::host_resources(4), false);
    press(&mut app, KeyCode::Char('m'));
    at_three_widths("machine_resources", &mut app);
}

#[test]
fn a_vaults_status_shows_in_the_machines_panel() {
    use herder_protocol::{HostId, HostReplication, VaultStatus};
    let mut app = fake::vault();
    let mut vault = app.machines[0].clone();
    vault.vault = Some(VaultStatus {
        sessions: 3,
        events: 1250,
        storage_bytes: 3 << 20,
        hosts: vec![
            HostReplication {
                host_id: HostId::new("devbox"),
                sessions: 2,
                events: 1000,
                last_event_at: Some(herder_protocol::Timestamp::now()),
                lag_ms: Some(40),
            },
            HostReplication {
                host_id: HostId::new("laptop"),
                sessions: 1,
                events: 250,
                last_event_at: None,
                lag_ms: None,
            },
        ],
    });
    app.update(Msg::Machines(vec![vault]));
    press(&mut app, KeyCode::Char('m'));
    let screen = render(&mut app, 160, 40).backend().to_string();
    assert!(
        screen.contains("vault         3 sessions · 1250 events · 3 MiB"),
        "{screen}"
    );
    assert!(
        screen.contains("devbox        2 sessions · 1000 events · last event 0m ago · lag 40 ms"),
        "{screen}"
    );
    assert!(
        screen.contains("laptop        1 session · 250 events"),
        "{screen}"
    );
}

#[test]
fn a_sessions_usage_wait_and_leftovers_on_narrow_and_wide_screens() {
    let mut app = open_s2(vec![fake::status(
        herder_protocol::SessionStatus::WaitingForCapacity,
    )]);
    fake::with_resources(&mut app, fake::host_resources(4), true);
    at_three_widths("session_resources", &mut app);
}

#[test]
fn resource_figures_are_redrawn_as_they_arrive() {
    let mut app = open_s2(Vec::new());
    let text = |app: &mut App| format!("{:?}", render(app, 120, 40).backend());
    // Nothing to show yet: no strip.
    assert!(!text(&mut app).contains("resources"));
    fake::with_resources(&mut app, fake::host_resources(2), true);
    let shown = text(&mut app);
    assert!(
        shown.contains("cpu 12% · mem 768 MiB · 3 processes"),
        "{shown}"
    );
    assert!(shown.contains(":down brings app down"), "{shown}");
    assert!(!shown.contains("waiting for capacity"), "{shown}");
    let mut busier = fake::host_resources(2);
    busier.cpu_percent = 97.0;
    fake::with_resources(&mut app, busier, false);
    let shown = text(&mut app);
    assert!(shown.contains("cpu    97%"), "{shown}");
    assert!(!shown.contains("3 processes"), "{shown}");
}

#[test]
fn the_agent_session_view_on_narrow_and_wide_screens() {
    // docs/tui-design.md §2.1: messages, tool lines and blocks, the reply's footer, the
    // spinner and usage under the prompt.
    for (width, height) in [(45, 40), (100, 30), (160, 40)] {
        let mut app = fake::chat();
        insta::assert_snapshot!(
            format!("chat_{width}x{height}"),
            render(&mut app, width, height).backend()
        );
    }
}

#[test]
fn the_item_cursor_expands_tool_output_and_splits_diffs_when_wide() {
    let mut app = fake::chat();
    app.focus = Focus::Transcript;
    for id in ["r1", "c3"] {
        app.chat.expanded.insert(ItemId::new(id));
    }
    app.chat.cursor = Some(ItemId::new("c3"));
    app.chat.reveal = true;
    insta::assert_snapshot!(render(&mut app, 160, 50).backend());
}

#[test]
fn an_approval_replaces_the_prompt_and_a_question_sits_over_it() {
    let mut app = fake::chat_approval();
    insta::assert_snapshot!("chat_approval", render(&mut app, 100, 30).backend());
    let mut app = fake::chat_question();
    insta::assert_snapshot!("chat_question", render(&mut app, 45, 40).backend());
}

#[test]
fn the_command_popup_opens_over_the_transcript() {
    let mut app = fake::chat();
    fake::type_text(&mut app, "/mo");
    insta::assert_snapshot!(render(&mut app, 100, 30).backend());
}

#[test]
fn the_command_palette_on_narrow_and_wide_screens() {
    let mut app = open_s2(vec![fake::started("turn-1")]);
    app.update(Msg::Key(KeyEvent::new(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL,
    )));
    assert!(app.compose.palette.is_some());
    narrow_and_wide("palette", &mut app);
    // Typing filters; names the search starts come first.
    fake::type_text(&mut app, "acc");
    insta::assert_snapshot!("palette_filtered", render(&mut app, 100, 20).backend());
}

#[test]
fn the_new_session_projects_on_narrow_and_wide_screens() {
    let mut app = fake::projects();
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('n'));
    narrow_and_wide("new_session_projects", &mut app);
}

#[test]
fn a_live_daemons_sessions_at_three_widths() {
    let mut app = fake::live();
    at_three_widths("live", &mut app);
    // The expanded tools, and the archived sessions shown.
    press(&mut app, KeyCode::Esc);
    for id in ["c3", "c4"] {
        app.chat.expanded.insert(herder_protocol::ItemId::new(id));
    }
    press(&mut app, KeyCode::Char('H'));
    at_three_widths("live_expanded", &mut app);
}

#[test]
fn sessions_are_named_by_their_first_prompt_and_archived_ones_hide() {
    let mut app = fake::live();
    let screen = render(&mut app, 160, 40).backend().to_string();
    // herder's own branches name nothing: the first prompt does, never the id.
    assert!(screen.contains("Fix the flaky reconnect"), "{screen}");
    let sidebar: String = screen
        .lines()
        .map(|line| line.chars().take(26).collect::<String>())
        .collect();
    assert!(!sidebar.contains("eq3z0kae"), "{screen}");
    // A branch someone named, without herder's prefix.
    assert!(sidebar.contains("p2d-6-secondary"), "{screen}");
    // Archived: counted, hidden until H.
    assert!(screen.contains("3 archived"), "{screen}");
    assert!(!screen.contains("Bump ratatui"), "{screen}");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('H'));
    let screen = render(&mut app, 160, 40).backend().to_string();
    assert!(screen.contains("Bump ratatui"), "{screen}");
}

#[test]
fn tool_calls_are_one_line_until_expanded_and_then_capped() {
    let mut app = fake::live();
    let screen = render(&mut app, 160, 40).backend().to_string();
    // One line, with how long it took; no output.
    let line = screen
        .lines()
        .find(|line| line.contains("cargo test -p herder-client-core reconnect"))
        .unwrap();
    assert!(line.contains("14s"), "{line}");
    assert!(!screen.contains("case_01"), "{screen}");
    assert!(!screen.contains("more lines"), "{screen}");
    app.chat.expanded.insert(herder_protocol::ItemId::new("c3"));
    let screen = render(&mut app, 160, 40).backend().to_string();
    assert!(screen.contains("case_10"), "{screen}");
    assert!(!screen.contains("case_11"), "{screen}");
    assert!(screen.contains("30 more lines"), "{screen}");
}

#[test]
fn a_failure_is_a_marker_and_muted_text_not_a_red_line() {
    let mut app = fake::live();
    let theme = crate::ui::theme::Theme::herder(crate::ui::theme::Mode::Dark);
    app.theme = theme.clone();
    let terminal = render(&mut app, 160, 40);
    let buffer = terminal.backend().buffer();
    for text in ["cargo clippy", "model opus-9"] {
        let (x, y) = find(buffer, text);
        assert_eq!(buffer[(x, y)].fg, theme.text_muted, "{text}");
        // The row's error colour is on its marker only.
        let red = (0..buffer.area.width)
            .filter(|&x| buffer[(x, y)].fg == theme.error && buffer[(x, y)].symbol() != " ")
            .count();
        assert!(red <= 2, "{text}: {red} red cells");
    }
}

#[test]
fn the_details_panel_wraps_rather_than_cuts_and_bars_take_one_row() {
    let mut app = fake::live();
    let screen = render(&mut app, 160, 40).backend().to_string();
    assert!(screen.contains("full_access"), "{screen}");
    assert!(screen.contains("~/Projects/herder-sh/herder"), "{screen}");
    // Thin bars: nothing that fills a cell to the next row.
    assert!(!screen.contains('█'), "{screen}");
    let bars: Vec<usize> = screen
        .lines()
        .filter(|line| line.contains("━") && line.contains('%'))
        .map(|line| line.chars().position(|c| c == '━').unwrap())
        .collect();
    assert!(
        bars.len() >= 2 && bars.windows(2).all(|w| w[0] == w[1]),
        "{bars:?}"
    );
    // Load as labels and values.
    assert!(screen.contains("cpu    44%"), "{screen}");
    assert!(screen.contains("mem    45%"), "{screen}");
}

#[test]
fn the_empty_prompts_cursor_sits_before_its_placeholder() {
    let mut app = fake::live();
    app.compose.errors.clear();
    let screen = render(&mut app, 100, 30).backend().to_string();
    assert!(screen.contains("▌Write a prompt"), "{screen}");
}

/// Where `text` starts in `buffer`.
fn find(buffer: &ratatui::buffer::Buffer, text: &str) -> (u16, u16) {
    let wanted: Vec<String> = text.chars().map(String::from).collect();
    for y in 0..buffer.area.height {
        let row: Vec<&str> = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        for x in 0..row.len().saturating_sub(wanted.len()) {
            if row[x..x + wanted.len()]
                .iter()
                .zip(&wanted)
                .all(|(a, b)| a == b)
            {
                return (u16::try_from(x).unwrap(), y);
            }
        }
    }
    panic!("no {text:?}");
}

/// [`TestBackend`] that counts its clears.
struct Clears(TestBackend, usize);

impl ratatui::backend::Backend for Clears {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.0.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.0.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        self.0.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.1 += 1;
        self.0.clear()
    }
    fn clear_region(&mut self, kind: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.1 += 1;
        self.0.clear_region(kind)
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.0.size()
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.0.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}

#[test]
fn a_resync_writes_every_cell_again_without_a_clear() {
    use ratatui::backend::Backend;
    use ratatui::buffer::Cell;

    let mut app = fake::tree();
    let mut terminal = Terminal::new(Clears(TestBackend::new(45, 40), 0)).unwrap();
    let mut last = ratatui::buffer::Buffer::default();
    assert!(super::paint(&mut terminal, &mut app, super::Paint::Diff, &mut last).unwrap());
    // The terminal lost track of the screen, as a phone app over mosh may.
    let mut junk = Cell::default();
    junk.set_symbol("#");
    let cells: Vec<(u16, u16, Cell)> = (0..40)
        .flat_map(|y| (0..45).map(move |x| (x, y)))
        .map(|(x, y)| (x, y, junk.clone()))
        .collect();
    terminal
        .backend_mut()
        .draw(cells.iter().map(|(x, y, cell)| (*x, *y, cell)))
        .unwrap();
    super::paint(&mut terminal, &mut app, super::Paint::Resync, &mut last).unwrap();
    assert_eq!(terminal.backend().1, 0, "a resync never clears");
    assert_eq!(*terminal.backend().0.buffer(), fresh(&mut app, 45, 40));
}

#[test]
fn an_unchanged_frame_writes_nothing() {
    let mut app = fake::tree();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut last = ratatui::buffer::Buffer::default();
    assert!(super::paint(&mut terminal, &mut app, super::Paint::Full, &mut last).unwrap());
    // Nothing changed: the terminal gets nothing, however often the loop asks.
    for _ in 0..3 {
        assert!(!super::paint(&mut terminal, &mut app, super::Paint::Diff, &mut last).unwrap());
    }
    // A change is written, and only the frame after it.
    app.update(Msg::Key(KeyEvent::new(
        KeyCode::Char('j'),
        KeyModifiers::NONE,
    )));
    assert!(super::paint(&mut terminal, &mut app, super::Paint::Diff, &mut last).unwrap());
    assert_eq!(*terminal.backend().buffer(), fresh(&mut app, 100, 30));
    assert!(!super::paint(&mut terminal, &mut app, super::Paint::Diff, &mut last).unwrap());
}

pub(super) fn limit_reset() -> App {
    let mut app = fake::chat();
    let key = app.open.clone().unwrap();
    let turn_id = app.open_session().unwrap().turn.clone().unwrap();
    let at = "2026-10-03T21:20:00Z".parse().unwrap();
    let mut machines = app.machines.clone();
    machines[0].accounts[0].usage[0].used_percent = 100.0;
    machines[0].accounts[0].usage[0].resets_at = Some(at);
    app.update(Msg::Machines(machines));
    fake::feed(
        &mut app,
        key.host_id.as_str(),
        key.session_id.as_str(),
        fake::update(
            key.session_id.as_str(),
            100,
            vec![
                herder_protocol::EventBody::TurnFailed {
                    turn_id,
                    error: herder_protocol::TurnError {
                        class: herder_protocol::ErrorClass::LimitReached,
                        message: "5-hour limit reached".into(),
                    },
                },
                herder_protocol::EventBody::SessionStatusChanged {
                    status: herder_protocol::SessionStatus::WaitingForCapacity,
                    retry_at: Some(at),
                },
            ],
            Vec::new(),
        ),
    );
    app
}

#[test]
fn usage_reset_wait_shows_its_deadline() {
    let app = limit_reset();
    let lines = super::resources::lines(&app, true);
    let shown = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        shown.contains("waiting for limit reset") && shown.contains("21:20 UTC"),
        "{shown}"
    );
    assert!(!shown.contains("waiting for capacity"), "{shown}");
}

#[test]
fn a_fork_reads_as_a_switch_to_its_machine() {
    let app = open_s2(vec![herder_protocol::EventBody::SessionForked {
        from_session: herder_protocol::SessionId::new("s0"),
        from_host: herder_protocol::HostId::new("laptop-id"),
    }]);
    let session = &app.sessions[&fake::key("h1", "s2")];
    let (rows, _) = super::transcript::rows(&app, session, 80);
    let text: Vec<String> = rows.iter().map(|row| row.line.to_string()).collect();
    assert!(
        text.iter()
            .any(|line| line.contains(" switched to box (from laptop-id) ")),
        "{text:?}"
    );
}
