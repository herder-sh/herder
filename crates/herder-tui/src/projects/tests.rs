use herder_protocol::{AccountId, CommandBody, PermissionMode};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{Effect, Msg};
use crate::compose::Origin;
use crate::fake::{self, key};
use crate::new_session::{Choice, Field, Step};

fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn session(host: &str, id: &str, depth: usize) -> Row {
    Row::Session {
        key: key(host, id),
        depth,
    }
}

fn project(id: &str) -> Row {
    Row::Project(Some(ProjectId::new(id)))
}

#[test]
fn clones_of_one_repo_on_two_machines_are_one_project_with_both_machines_sessions() {
    let app = fake::projects();
    assert_eq!(app.grouping, Grouping::Projects);
    assert_eq!(
        app.rows(),
        [
            project("github.com/acme/app"),
            session("h2", "s4", 0),
            session("h2", "s6", 1),
            session("h1", "s3", 0),
            session("h1", "s1", 0),
            project("github.com/acme/docs"),
            session("h1", "s2", 0),
            project("h2:/work/scratch"),
            session("h2", "s5", 0),
        ]
    );
}

#[test]
fn project_names_are_the_last_segment_of_their_id() {
    assert_eq!(name(&ProjectId::new("github.com/acme/app")), "app");
    assert_eq!(name(&ProjectId::new("h2:/work/scratch")), "scratch");
    assert_eq!(name(&ProjectId::new("h2:/work/scratch/")), "scratch");
}

#[test]
fn v_groups_by_machine_and_back_keeping_the_selected_session() {
    let mut app = fake::projects();
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected(), Some(session("h2", "s6", 1)));
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.grouping, Grouping::Machines);
    assert_eq!(
        app.rows(),
        [
            Row::Machine(HostId::new("h1")),
            session("h1", "s3", 0),
            session("h1", "s2", 0),
            session("h1", "s1", 0),
            Row::Machine(HostId::new("h2")),
            session("h2", "s5", 0),
            session("h2", "s4", 0),
            session("h2", "s6", 1),
        ]
    );
    assert_eq!(app.selected(), Some(session("h2", "s6", 1)));
    // A heading has no place in the other grouping: the selection starts over.
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.grouping, Grouping::Projects);
    assert_eq!(app.selected(), Some(session("h2", "s4", 0)));
}

#[test]
fn a_session_joins_its_project_once_its_daemon_resolves_it() {
    let mut app = fake::projects();
    let s5 = key("h2", "s5");
    assert_eq!(
        app.project_of(&s5),
        Some(ProjectId::new("h2:/work/scratch"))
    );
    let mut machines = app.machines.clone();
    machines[1].sessions[1] = fake::head("s5", Some("github.com/acme/app"));
    app.update(Msg::Machines(machines));
    assert_eq!(app.rows()[1], session("h2", "s5", 0));
    // A session not loaded yet has no repo, so no project until it is.
    let mut machines = app.machines.clone();
    machines[0].sessions.push(fake::head("s7", None));
    app.update(Msg::Machines(machines));
    assert_eq!(app.project_of(&key("h1", "s7")), None);
    assert_eq!(
        app.rows()[app.rows().len() - 2..],
        [Row::Project(None), session("h1", "s7", 0)]
    );
}

#[test]
fn a_new_session_starts_from_the_project_on_the_clone_used_last() {
    let mut app = fake::projects();
    assert_eq!(
        app.clones(&ProjectId::new("github.com/acme/app")),
        [
            ProjectClone {
                host_id: HostId::new("h1"),
                repo: "/home/ann/src/app".into(),
            },
            ProjectClone {
                host_id: HostId::new("h2"),
                repo: "/work/app".into(),
            },
        ]
    );
    // On the project's heading; its newest session, s6, runs on laptop: the dialog starts at
    // the project's machines, on that clone.
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('n'));
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!(dialog.project, Some(ProjectId::new("github.com/acme/app")));
    assert_eq!(dialog.step, Step::Machine);
    assert_eq!(
        app.new_session_choices(),
        [
            Choice::Machine(HostId::new("h1"), Some("/home/ann/src/app".into())),
            Choice::Machine(HostId::new("h2"), Some("/work/app".into())),
        ]
    );
    assert_eq!(dialog.selected, 1);

    // Each machine with a clone, each with its own path.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!(dialog.host_id, HostId::new("h1"));
    assert_eq!(dialog.repo.lines(), ["/home/ann/src/app"]);
    assert_eq!(dialog.field, Field::Account);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        [Effect::Send {
            host_id: HostId::new("h1"),
            command: CommandBody::CreateSession {
                repo: Some("/home/ann/src/app".into()),
                project_id: None,
                branch: None,
                account_id: Some(AccountId::new("claude-main")),
                provider: None,
                model: None,
                permission_mode: Some(PermissionMode::Ask),
                max_children: None,
                failover_pin: None,
            },
            origin: Origin::NewSession(HostId::new("h1")),
        }]
    );
}

#[test]
fn a_new_session_from_a_projects_session_uses_that_project() {
    let mut app = fake::projects();
    // s2, of acme/docs, which only box has.
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('k'));
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.selected(), Some(session("h1", "s2", 0)));
    press(&mut app, KeyCode::Char('n'));
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!(dialog.project, Some(ProjectId::new("github.com/acme/docs")));
    assert_eq!(
        app.new_session_choices(),
        [Choice::Machine(
            HostId::new("h1"),
            Some("/home/ann/src/docs".into())
        )]
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.compose.dialog.as_ref().unwrap().host_id,
        HostId::new("h1")
    );

    // Grouped by machine, the dialog starts at the projects.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('n'));
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!((dialog.step, &dialog.project), (Step::Project, &None));
}

#[test]
fn the_pr_view_lists_each_projects_prs_across_sessions_and_machines() {
    let mut app = fake::projects();
    press(&mut app, KeyCode::Char('P'));
    let all: Vec<(SessionKey, u64)> = app
        .all_prs()
        .into_iter()
        .map(|(key, pr)| (key.clone(), pr.number))
        .collect();
    assert_eq!(
        all,
        [
            (key("h2", "s4"), 12),
            (key("h1", "s3"), 7),
            (key("h1", "s2"), 3)
        ]
    );
}

#[test]
fn listed_projects_name_themselves_offer_every_clone_and_their_default_account() {
    let mut app = fake::projects();
    let app_id = ProjectId::new("github.com/acme/app");
    let mut machines = app.machines.clone();
    // laptop names the project and has a second clone with no session yet, and starts its
    // sessions on a second account.
    machines[1]
        .accounts
        .push(fake::account("claude-work", "Work"));
    machines[1].projects = vec![herder_protocol::Project {
        project_id: app_id.clone(),
        name: "Acme app".into(),
        paths: vec!["/work/app".into(), "/srv/app".into()],
        default_permission_mode: Some(herder_protocol::PermissionMode::AutoEdit),
        default_account: Some(AccountId::new("claude-work")),
        setup_command: None,
        icon: None,
        icon_uploaded: false,
    }];
    app.update(Msg::Machines(machines));
    assert_eq!(app.project_name(&app_id), "Acme app");
    assert_eq!(
        app.project_name(&ProjectId::new("github.com/acme/docs")),
        "docs"
    );
    assert_eq!(
        app.clones(&app_id),
        [
            ProjectClone {
                host_id: HostId::new("h1"),
                repo: "/home/ann/src/app".into(),
            },
            ProjectClone {
                host_id: HostId::new("h2"),
                repo: "/work/app".into(),
            },
            ProjectClone {
                host_id: HostId::new("h2"),
                repo: "/srv/app".into(),
            },
        ]
    );

    // On laptop, the dialog starts on the project's default account and permission mode; on
    // box, the first account.
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!((&dialog.host_id, dialog.account), (&HostId::new("h2"), 1));
    assert_eq!(dialog.mode, herder_protocol::PermissionMode::AutoEdit);
    assert_eq!(dialog.repo.lines(), ["/work/app"]);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!(dialog.repo.lines(), ["/srv/app"]);
    assert_eq!(dialog.account, 1);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Enter);
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!((&dialog.host_id, dialog.account), (&HostId::new("h1"), 0));
}
