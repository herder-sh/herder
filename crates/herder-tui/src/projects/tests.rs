use herder_protocol::{AccountId, CommandBody, PermissionMode, SessionHead, SessionId};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{Effect, Msg};
use crate::compose::{Field, Origin};
use crate::fake::{self, key};

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
    machines[1].sessions[1] = SessionHead {
        session_id: SessionId::new("s5"),
        head_seq: 0,
        project_id: Some(ProjectId::new("github.com/acme/app")),
    };
    app.update(Msg::Machines(machines));
    assert_eq!(app.rows()[1], session("h2", "s5", 0));
    // A session not loaded yet has no repo, so no project until it is.
    let mut machines = app.machines.clone();
    machines[0].sessions.push(SessionHead {
        session_id: SessionId::new("s7"),
        head_seq: 0,
        project_id: None,
    });
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
    // On the project's heading; its newest session, s6, runs on laptop.
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('n'));
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!(dialog.project, Some(ProjectId::new("github.com/acme/app")));
    assert_eq!(dialog.field, Field::Machine);
    assert_eq!(dialog.host_id, HostId::new("h2"));
    assert_eq!(dialog.repo.lines(), ["/work/app"]);

    // The machine field picks among the machines with a clone, each with its own path.
    press(&mut app, KeyCode::Right);
    let dialog = app.compose.dialog.as_ref().unwrap();
    assert_eq!(dialog.host_id, HostId::new("h1"));
    assert_eq!(dialog.repo.lines(), ["/home/ann/src/app"]);
    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.compose.dialog.as_ref().unwrap().host_id,
        HostId::new("h2")
    );
    press(&mut app, KeyCode::Left);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        [Effect::Send {
            host_id: HostId::new("h1"),
            command: CommandBody::CreateSession {
                repo: "/home/ann/src/app".into(),
                branch: None,
                account_id: AccountId::new("claude-main"),
                model: None,
                permission_mode: PermissionMode::Ask,
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
    assert_eq!(dialog.repo.lines(), ["/home/ann/src/docs"]);
    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.compose.dialog.as_ref().unwrap().host_id,
        HostId::new("h1")
    );

    // Grouped by machine, the dialog is the plain one.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(app.compose.dialog.as_ref().unwrap().project, None);
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
