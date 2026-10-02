//! Snapshots of the main screens, drawn on a test backend from fake client-core state.

use herder_client_core::ConnectionState;
use herder_protocol::{ItemBody, ItemId};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde_json::json;

use crate::app::Focus;
use crate::app::{App, Msg};
use crate::fake::{self, added, assistant, item, update};

fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal
}

fn press(app: &mut App, code: KeyCode) {
    app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// `s2` of [`fake::tree`] mid-turn: a prompt, some tool work, and an answer streaming.
fn mid_turn() -> App {
    let mut app = fake::tree();
    let events = vec![
        added(
            "i1",
            ItemBody::UserMessage {
                text: "Add a health endpoint and test it.".into(),
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
    app
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
    // Badges carry their colour: "needs you" stands out.
    let buffer = terminal.backend().buffer();
    let needs_you = (0..80).find(|&x| buffer[(x, 3)].symbol() == "n").unwrap();
    assert_eq!(buffer[(needs_you, 3)].fg, Color::Magenta);
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
            },
        ),
        fake::approval("a1", "Bash: rm -rf target"),
        fake::approval("a2", "Bash: cargo build"),
    ]);
    let terminal = render(&mut app, 100, 20);
    insta::assert_snapshot!(terminal.backend());
    let screen = terminal.backend().to_string();
    assert!(screen.contains("approve?"), "{screen}");
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
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    insta::assert_snapshot!(render(&mut app, 100, 20).backend());
}

#[test]
fn the_session_list_badges_each_sessions_prs() {
    let mut app = fake::with_prs();
    let terminal = render(&mut app, 80, 10);
    insta::assert_snapshot!(terminal.backend());
    // The badge's number takes its PR state's colour, its marks the checks'.
    let buffer = terminal.backend().buffer();
    let row = |y: u16| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    let s2 = (0..12).find(|&y| row(y).contains("#7")).unwrap();
    let at = row(s2).find("#7").unwrap();
    let x = u16::try_from(row(s2)[..at].chars().count()).unwrap();
    assert_eq!(buffer[(x, s2)].fg, Color::Green);
    assert_eq!(buffer[(x + 2, s2)].symbol(), "✓");
}

#[test]
fn the_open_session_shows_its_prs_over_the_transcript() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('j'));
    insta::assert_snapshot!(render(&mut app, 110, 14).backend());
    // Out of the strip, the strip stays and only hints at its key.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Transcript);
    let terminal = render(&mut app, 110, 14);
    let screen = terminal.backend().to_string();
    assert!(screen.contains("pull requests (2)"), "{screen}");
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
    assert!(!screen.contains("connected"), "{screen}");
    press(&mut app, KeyCode::Char('j'));
    let screen = render(&mut app, 90, 8).backend().to_string();
    assert!(screen.contains("connected"), "{screen}");
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

const LINK: &str = "herder://pair?host=192.168.1.5%3A7447&host=10.0.0.2%3A7447\
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
fn the_add_account_dialog_picks_a_provider_and_names_the_account() {
    let mut app = fake::tree();
    let mut machines = app.machines.clone();
    machines[0].accounts = vec![herder_protocol::Account {
        account_id: herder_protocol::AccountId::new("claude-main"),
        provider: herder_protocol::Provider::Claude,
        label: "Main".into(),
        usage: Vec::new(),
        failover: false,
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
    let screen = render(&mut app, 80, 10).backend().to_string();
    assert!(screen.contains("(2) !1"), "{screen}");
    // Folded, the badge still tells.
    press(&mut app, KeyCode::Char('z'));
    insta::assert_snapshot!(render(&mut app, 80, 10).backend());
}

#[test]
fn the_inbox_shows_each_request_with_its_task_reason_and_note() {
    let mut app = fake::escalated();
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
        insta::assert_snapshot!(
            format!("{name}_{width}x{height}"),
            render(app, width, height).backend()
        );
    }
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

#[test]
fn the_inbox_on_narrow_and_wide_screens() {
    let mut app = fake::escalated();
    press(&mut app, KeyCode::Char('i'));
    narrow_and_wide("inbox", &mut app);
}

#[test]
fn every_pr_on_narrow_and_wide_screens() {
    let mut app = fake::with_prs();
    press(&mut app, KeyCode::Char('P'));
    narrow_and_wide("prs", &mut app);
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
    super::paint(terminal, app, true).unwrap();
}

#[test]
fn resizing_narrow_wide_narrow_leaves_no_stale_cells() {
    let mut app = mid_turn();
    let mut terminal = Terminal::new(TestBackend::new(45, 40)).unwrap();
    super::paint(&mut terminal, &mut app, false).unwrap();
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
    super::paint(&mut terminal, &mut app, false).unwrap();
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

/// [`fake::tree`] whose machine has three accounts, two with usage and `claude-work` opted in
/// to failover, and pins its sessions and fails over to codex, beside a disconnected machine
/// with none; `s2` and `s3` run on `claude-main`.
fn with_accounts() -> App {
    use herder_protocol::{Provider, Timestamp, UsageWindow};

    // Half a minute past each reset time, so the countdown reads the same while the test runs.
    let window = |name: &str, used_percent: f64, secs: i64| UsageWindow {
        window: name.into(),
        used_percent,
        resets_at: Some(Timestamp::from_second(Timestamp::now().as_second() + secs + 30).unwrap()),
    };
    let mut app = fake::tree();
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
    let mut work = fake::account("claude-work", "Work");
    work.failover = true;
    machines[0].accounts = vec![main, work, codex];
    machines[0].failover = herder_protocol::FailoverSettings {
        pin: true,
        providers: vec![Provider::Codex],
    };
    let mut laptop = fake::machine("h2", "laptop", &[]);
    laptop.connection = ConnectionState::Disconnected {
        error: "connection refused".into(),
    };
    machines.push(laptop);
    app.update(Msg::Machines(machines));
    app
}

#[test]
fn the_accounts_screen_on_narrow_and_wide_screens() {
    let mut app = with_accounts();
    press(&mut app, KeyCode::Char('A'));
    narrow_and_wide("accounts", &mut app);
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
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
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
    narrow_and_wide("project_prs", &mut app);
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
    narrow_and_wide("machine_resources", &mut app);
}

#[test]
fn a_sessions_usage_wait_and_leftovers_on_narrow_and_wide_screens() {
    let mut app = open_s2(vec![fake::status(
        herder_protocol::SessionStatus::WaitingForCapacity,
    )]);
    fake::with_resources(&mut app, fake::host_resources(4), true);
    narrow_and_wide("session_resources", &mut app);
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
    assert!(shown.contains("cpu  97%"), "{shown}");
    assert!(!shown.contains("3 processes"), "{shown}");
}
