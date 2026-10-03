//! Projections: the read models each appended event updates, inside the append's transaction.

use herder_protocol::{Event, EventBody, PullRequest, SessionId, SessionStatus};
use rusqlite::{Transaction, params};

use crate::{Result, tag};

/// Applies `event` to the `sessions`, `session_prs` and `session_branches` read models.
pub(crate) fn apply(tx: &Transaction<'_>, event: &Event) -> Result<()> {
    let id = event.session_id.as_str();
    match &event.body {
        EventBody::SessionCreated {
            repo,
            worktree,
            branch,
            provider,
            account_id,
            model,
            permission_mode,
            parent,
            task,
            ..
        } => {
            tx.prepare_cached(
                "INSERT INTO sessions (session_id, repo, worktree, branch, provider, account_id,
                     model, permission_mode, parent, task, status, last_seq, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            )?
            .execute(params![
                id,
                repo,
                worktree,
                branch,
                provider.as_str(),
                account_id.as_str(),
                model,
                tag(permission_mode)?,
                parent.as_ref().map(SessionId::as_str),
                task,
                tag(&SessionStatus::Idle)?,
                event.seq,
                event.at,
            ])?;
            add_branch(tx, id, branch, event.seq)?;
        }
        EventBody::BranchCheckedOut { branch } => add_branch(tx, id, branch, event.seq)?,
        EventBody::SessionStatusChanged { status } => {
            tx.prepare_cached("UPDATE sessions SET status = ?2 WHERE session_id = ?1")?
                .execute(params![id, tag(status)?])?;
        }
        EventBody::ModelSwitched { model } => {
            tx.prepare_cached("UPDATE sessions SET model = ?2 WHERE session_id = ?1")?
                .execute(params![id, model])?;
        }
        EventBody::AccountSwitched { account_id } => {
            tx.prepare_cached("UPDATE sessions SET account_id = ?2 WHERE session_id = ?1")?
                .execute(params![id, account_id.as_str()])?;
        }
        EventBody::ProviderSwitched {
            provider,
            account_id,
            model,
        } => {
            tx.prepare_cached(
                "UPDATE sessions SET provider = ?2, account_id = ?3, model = ?4
                 WHERE session_id = ?1",
            )?
            .execute(params![id, provider.as_str(), account_id.as_str(), model])?;
        }
        EventBody::PermissionModeChanged { mode } => {
            tx.prepare_cached("UPDATE sessions SET permission_mode = ?2 WHERE session_id = ?1")?
                .execute(params![id, tag(mode)?])?;
        }
        EventBody::TitleChanged { title, source } => {
            tx.prepare_cached(
                "UPDATE sessions SET title = ?2, title_source = ?3 WHERE session_id = ?1",
            )?
            .execute(params![id, title, tag(source)?])?;
        }
        EventBody::PrLinked { pr } => {
            write_pr(
                tx,
                "INSERT INTO session_prs (session_id, number, url, title, state, ci, review,
                     mergeable, head_branch)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (session_id, number) DO UPDATE SET url = excluded.url,
                     title = excluded.title, state = excluded.state, ci = excluded.ci,
                     review = excluded.review, mergeable = excluded.mergeable,
                     head_branch = excluded.head_branch",
                id,
                pr,
            )?;
        }
        // An update for a pull request that is not tracked (e.g. it arrives after an unlink) is
        // journaled but does not bring the pull request back.
        EventBody::PrUpdated { pr } => {
            write_pr(
                tx,
                "UPDATE session_prs SET url = ?3, title = ?4, state = ?5, ci = ?6, review = ?7,
                     mergeable = ?8, head_branch = ?9
                 WHERE session_id = ?1 AND number = ?2",
                id,
                pr,
            )?;
        }
        EventBody::PrUnlinked { number } => {
            tx.prepare_cached("DELETE FROM session_prs WHERE session_id = ?1 AND number = ?2")?
                .execute(params![id, number])?;
        }
        EventBody::TurnStarted { .. }
        | EventBody::TurnCompleted { .. }
        | EventBody::TurnInterrupted { .. }
        | EventBody::TurnFailed { .. }
        | EventBody::ItemAdded { .. }
        | EventBody::ApprovalRequested { .. }
        | EventBody::ApprovalEscalated { .. }
        | EventBody::ApprovalResolved { .. }
        | EventBody::QuestionAsked { .. }
        | EventBody::QuestionEscalated { .. }
        | EventBody::QuestionAnswered { .. }
        | EventBody::ChildSpawned { .. }
        | EventBody::ChildReported { .. }
        | EventBody::Unknown => {}
    }
    tx.prepare_cached("UPDATE sessions SET last_seq = ?2, updated_at = ?3 WHERE session_id = ?1")?
        .execute(params![id, event.seq, event.at])?;
    Ok(())
}

/// Records that the session owns `branch`; a branch it already owns keeps its first seq.
fn add_branch(tx: &Transaction<'_>, session_id: &str, branch: &str, seq: u64) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO session_branches (session_id, branch, first_seen_seq) VALUES (?1, ?2, ?3)
         ON CONFLICT (session_id, branch) DO NOTHING",
    )?
    .execute(params![session_id, branch, seq])?;
    Ok(())
}

fn write_pr(tx: &Transaction<'_>, sql: &str, session_id: &str, pr: &PullRequest) -> Result<()> {
    tx.prepare_cached(sql)?.execute(params![
        session_id,
        pr.number,
        pr.url,
        pr.title,
        tag(&pr.state)?,
        tag(&pr.ci)?,
        tag(&pr.review)?,
        tag(&pr.mergeable)?,
        pr.head_branch,
    ])?;
    Ok(())
}
