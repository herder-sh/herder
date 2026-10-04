//! [`crate::Client::fork_session`]: forking a session of one machine onto another, picking
//! where the destination finds the session's history.

use std::sync::Arc;
use std::time::Duration;

use herder_protocol::{
    AccountId, AttachmentId, Command, CommandBody, CommandResult, ErrorCode, ErrorInfo, Event,
    EventBody, HistoryPart, HostId, Image, ItemBody, Relay, SessionId,
};

use crate::supervisor::{Subscription, Supervisor};
use crate::{ConnectionState, Error, new_command_id};

/// How long reading a session from its machine may take, a long journal included.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Most bytes of events one `upload_history` part carries, as JSON; an event larger than
/// this goes in a part of its own. Well under a daemon's 16 MiB WebSocket frame limit.
const PART_BYTES: usize = 4 * 1024 * 1024;

/// Forks `session_id`, listed by `source`, onto `destination`.
pub(crate) async fn fork(
    source: Option<Arc<Supervisor>>,
    source_id: HostId,
    destination: Arc<Supervisor>,
    session_id: SessionId,
    account_id: Option<AccountId>,
) -> Result<CommandResult, Error> {
    let fork = |relay| CommandBody::ForkSession {
        session_id: session_id.clone(),
        account_id: account_id.clone(),
        relay,
    };
    let view = source.as_ref().map(|source| source.view());
    let head = view
        .as_ref()
        .and_then(|view| view.sessions.iter().find(|s| s.session_id == session_id));
    // A vault lists sessions of other hosts; a daemon's are its own.
    let host_id = head
        .and_then(|head| head.host_id.clone())
        .unwrap_or_else(|| source_id.clone());
    if host_id == destination.saved.host_id {
        return send(&destination, fork(None)).await;
    }
    let connected = view
        .as_ref()
        .is_some_and(|view| view.connection == ConnectionState::Connected);
    if let (Some(source), Some(head), true) = (&source, head, connected) {
        let Some(project_id) = head.project_id.clone() else {
            return Err(rejected(
                ErrorCode::Conflict,
                format!(
                    "{} has not resolved the project of this session yet",
                    head_name(view.as_ref(), &source_id)
                ),
            ));
        };
        let (events, images) = tokio::time::timeout(READ_TIMEOUT, read(source, &session_id))
            .await
            .map_err(|_| {
                rejected(
                    ErrorCode::Internal,
                    format!(
                        "{} did not send the session in {READ_TIMEOUT:?}",
                        head_name(view.as_ref(), &source_id)
                    ),
                )
            })??;
        upload(&destination, &session_id, events, images).await?;
        return send(
            &destination,
            fork(Some(Relay {
                host_id,
                project_id,
            })),
        )
        .await;
    }
    // The source is unavailable: the destination's vault is the only other copy.
    let link = send(&destination, CommandBody::GetVaultLink).await?;
    if !matches!(link, CommandResult::VaultLink { vault: Some(_), .. }) {
        return Err(rejected(
            ErrorCode::NotFound,
            format!(
                "{} is offline and {} has no vault to find the session in",
                head_name(view.as_ref(), &source_id),
                destination.view().name
            ),
        ));
    }
    send(&destination, fork(None)).await
}

/// The whole journal of `session_id` as `source` holds it, and every image its prompts carried
/// that `source` still has.
async fn read(
    source: &Arc<Supervisor>,
    session_id: &SessionId,
) -> Result<(Vec<Event>, Vec<(AttachmentId, Image)>), Error> {
    let subscription = Subscription::new(Arc::clone(source), session_id.clone());
    source.synced().await?;
    let events = subscription.next().await.ok_or(Error::Closed)?.events;
    drop(subscription);
    if events.first().is_none_or(|event| event.seq != 1) {
        return Err(rejected(
            ErrorCode::NotFound,
            format!(
                "{} did not send the history of {session_id}",
                source.view().name
            ),
        ));
    }
    let mut images = Vec::new();
    for event in &events {
        let EventBody::ItemAdded { item } = &event.body else {
            continue;
        };
        let ItemBody::UserMessage { attachments, .. } = &item.body else {
            continue;
        };
        for attachment in attachments {
            let command = CommandBody::GetAttachment {
                session_id: session_id.clone(),
                attachment_id: attachment.attachment_id.clone(),
            };
            match send(source, command).await {
                Ok(CommandResult::Attachment { media_type, data }) => {
                    images.push((attachment.attachment_id.clone(), Image { media_type, data }));
                }
                // An image its machine lost is left out, as a vault copy leaves it out.
                Err(Error::Rejected { info }) if info.code == ErrorCode::NotFound => {}
                Err(err) => return Err(err),
                Ok(_) => return Err(unexpected(source)),
            }
        }
    }
    Ok((events, images))
}

/// Uploads a history to `destination` in parts: batches of events, then one image a part.
async fn upload(
    destination: &Supervisor,
    session_id: &SessionId,
    events: Vec<Event>,
    images: Vec<(AttachmentId, Image)>,
) -> Result<(), Error> {
    let mut parts = Vec::new();
    let (mut batch, mut bytes) = (Vec::new(), 0);
    for event in events {
        let size = serde_json::to_vec(&event).map_or(0, |json| json.len());
        if !batch.is_empty() && bytes + size > PART_BYTES {
            parts.push(HistoryPart::Events {
                events: std::mem::take(&mut batch),
            });
            bytes = 0;
        }
        batch.push(event);
        bytes += size;
    }
    parts.push(HistoryPart::Events { events: batch });
    parts.extend(
        images
            .into_iter()
            .map(|(attachment_id, image)| HistoryPart::Image {
                attachment_id,
                image,
            }),
    );
    for part in parts {
        let command = CommandBody::UploadHistory {
            session_id: session_id.clone(),
            part,
        };
        send(destination, command).await?;
    }
    Ok(())
}

async fn send(machine: &Supervisor, body: CommandBody) -> Result<CommandResult, Error> {
    let command = Command {
        id: new_command_id(),
        body,
    };
    machine
        .send(command)
        .await?
        .map_err(|info| Error::Rejected { info })
}

/// The name the source machine is shown by, else its host id.
fn head_name(view: Option<&crate::Machine>, host_id: &HostId) -> String {
    view.map_or_else(|| host_id.to_string(), |view| view.name.clone())
}

fn rejected(code: ErrorCode, message: String) -> Error {
    Error::Rejected {
        info: ErrorInfo { code, message },
    }
}

fn unexpected(machine: &Supervisor) -> Error {
    rejected(
        ErrorCode::Internal,
        format!("{} sent an unexpected answer", machine.view().name),
    )
}
