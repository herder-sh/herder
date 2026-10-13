//! [`crate::Client::fork_session`]: forking a session of one machine onto another, picking
//! where the destination finds the session's history.

use std::sync::Arc;
use std::time::Duration;

use herder_protocol::{
    AccountId, Attachment, Bytes, Command, CommandBody, CommandResult, ErrorCode, ErrorInfo, Event,
    HistoryPart, HostId, Image, Relay, SessionHead, SessionId,
};

use crate::supervisor::{Subscription, Supervisor};
use crate::{ConnectionState, Error, new_command_id};

/// How long reading a session from its machine may take, a long journal included.
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Most bytes of events one `upload_history` part carries, as JSON; an event larger than
/// this goes in a part of its own. Well under a daemon's 16 MiB WebSocket frame limit.
const PART_BYTES: usize = 4 * 1024 * 1024;

/// Forks `session_id`, listed by the machine `source_id`, onto `destination`; `machines` are
/// all of the client's, which may relay the history.
pub(crate) async fn fork(
    machines: Vec<Arc<Supervisor>>,
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
    let source = machines
        .iter()
        .find(|machine| machine.saved.host_id == source_id);
    let view = source.map(|source| source.view());
    // A vault lists sessions of other hosts; a daemon's are its own.
    let host_id = view
        .as_ref()
        .and_then(|view| listed(view, &session_id))
        .and_then(|head| head.host_id.clone())
        .unwrap_or_else(|| source_id.clone());
    if host_id == destination.saved.host_id {
        return send(&destination, fork(None)).await;
    }
    // Any connected machine that lists the session of that host relays it: the source first,
    // then the session's own machine, then a vault.
    let mut relays = machines
        .iter()
        .filter(|machine| machine.saved.host_id != destination.saved.host_id)
        .collect::<Vec<_>>();
    relays.sort_by_key(|machine| {
        (
            machine.saved.host_id != source_id,
            machine.saved.host_id != host_id,
        )
    });
    let relay = relays.into_iter().find_map(|machine| {
        let view = machine.view();
        let head = listed(&view, &session_id)?;
        let lists = head.host_id.as_ref().unwrap_or(&view.host_id) == &host_id;
        (lists && view.connection == ConnectionState::Connected)
            .then(|| (machine, view.name.clone(), head.project_id.clone()))
    });
    if let Some((relay, name, project_id)) = relay {
        let Some(project_id) = project_id else {
            return Err(rejected(
                ErrorCode::Conflict,
                format!("{name} has not resolved the project of this session yet"),
            ));
        };
        let (events, attachments) = tokio::time::timeout(READ_TIMEOUT, read(relay, &session_id))
            .await
            .map_err(|_| {
                rejected(
                    ErrorCode::Internal,
                    format!("{name} did not send the session in {READ_TIMEOUT:?}"),
                )
            })??;
        upload(&destination, &session_id, events, attachments).await?;
        return send(
            &destination,
            fork(Some(Relay {
                host_id,
                project_id,
            })),
        )
        .await;
    }
    // No machine the client reaches has it: the destination's vault is the only other copy.
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

/// The head of `session_id` among the sessions `view` lists.
fn listed<'a>(view: &'a crate::Machine, session_id: &SessionId) -> Option<&'a SessionHead> {
    view.sessions.iter().find(|s| &s.session_id == session_id)
}

/// The whole journal of `session_id` as `source` holds it, and every image and file its
/// prompts carried that `source` still has.
async fn read(
    source: &Arc<Supervisor>,
    session_id: &SessionId,
) -> Result<(Vec<Event>, Vec<(Attachment, Bytes)>), Error> {
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
    let mut kept = Vec::new();
    for event in &events {
        for attachment in event.body.attachments() {
            let command = CommandBody::GetAttachment {
                session_id: session_id.clone(),
                attachment_id: attachment.attachment_id.clone(),
            };
            match send(source, command).await {
                Ok(CommandResult::Attachment { data, .. }) => {
                    kept.push((attachment.clone(), data));
                }
                // One its machine lost is left out, as a vault copy leaves it out.
                Err(Error::Rejected { info }) if info.code == ErrorCode::NotFound => {}
                Err(err) => return Err(err),
                Ok(_) => return Err(unexpected(source)),
            }
        }
    }
    Ok((events, kept))
}

/// Uploads a history to `destination` in [`parts`].
async fn upload(
    destination: &Supervisor,
    session_id: &SessionId,
    events: Vec<Event>,
    attachments: Vec<(Attachment, Bytes)>,
) -> Result<(), Error> {
    for part in parts(events, attachments) {
        let command = CommandBody::UploadHistory {
            session_id: session_id.clone(),
            part,
        };
        send(destination, command).await?;
    }
    Ok(())
}

/// A history's `upload_history` parts: batches of events, then one image or file a part.
fn parts(events: Vec<Event>, attachments: Vec<(Attachment, Bytes)>) -> Vec<HistoryPart> {
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
    parts.extend(attachments.into_iter().map(|(attachment, data)| {
        if attachment.name.is_some() {
            return HistoryPart::File { attachment, data };
        }
        HistoryPart::Image {
            attachment_id: attachment.attachment_id,
            image: Image {
                media_type: attachment.media_type,
                data,
            },
        }
    }));
    parts
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

#[cfg(test)]
mod tests {
    use herder_protocol::{AttachmentId, FILE_MEDIA_TYPE};

    use super::*;

    #[test]
    fn images_go_up_as_images_and_files_as_files() {
        let image = Attachment {
            attachment_id: AttachmentId::new("a1"),
            media_type: "image/png".into(),
            size: 4,
            name: None,
        };
        let file = Attachment {
            attachment_id: AttachmentId::new("a2"),
            media_type: FILE_MEDIA_TYPE.into(),
            size: 3,
            name: Some("data.csv".into()),
        };
        let png = Bytes(b"\x89PNG".to_vec());
        let csv = Bytes(b"a,b".to_vec());
        let attachments = vec![(image, png.clone()), (file.clone(), csv.clone())];
        assert_eq!(
            parts(Vec::new(), attachments),
            [
                HistoryPart::Events { events: Vec::new() },
                HistoryPart::Image {
                    attachment_id: AttachmentId::new("a1"),
                    image: Image {
                        media_type: "image/png".into(),
                        data: png,
                    },
                },
                HistoryPart::File {
                    attachment: file,
                    data: csv,
                },
            ]
        );
    }
}
