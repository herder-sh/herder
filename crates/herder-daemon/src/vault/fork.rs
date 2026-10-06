//! Forking a session of another host, found in the vault this host replicates to: this host
//! reads the session's journal and the images its prompts carried from the vault as a client
//! ([`client`]), and the session manager makes the fork ([`crate::session::fork`]). The vault
//! tells which host the session runs on, up or gone; nothing is asked of that host.
//! Checkpoints its host could only bundle stay there: bundles are not uploaded to the vault.

use herder_client_core::auth::DeviceKey;
use herder_protocol::{ErrorCode, ErrorInfo, SessionId};

use super::client;
use crate::config::VaultConfig;
use crate::session::fork::Source;

/// Reads sessions to fork from the vault.
pub struct FromVault {
    /// The vault.
    pub vault: VaultConfig,
    /// The key this host is paired with the vault as.
    pub device: DeviceKey,
}

impl FromVault {
    /// `session_id` as the vault holds it.
    pub(crate) async fn source(&self, session_id: &SessionId) -> Result<Source, ErrorInfo> {
        let read = async |journal| {
            client::read(&self.vault, &self.device, journal)
                .await
                .map_err(|err| {
                    error(
                        ErrorCode::Internal,
                        format!("cannot read the vault: {err:#}"),
                    )
                })
        };
        // The list first: the vault refuses the journal of a session it does not hold.
        let listed = read(None).await?;
        if !listed.sessions.iter().any(|s| s.session_id == *session_id) {
            return Err(error(
                ErrorCode::NotFound,
                format!("neither this host nor its vault holds session {session_id}"),
            ));
        }
        let view = read(Some(session_id)).await?;
        let Some(head) = view.sessions.iter().find(|s| s.session_id == *session_id) else {
            return Err(error(
                ErrorCode::NotFound,
                format!("neither this host nor its vault holds session {session_id}"),
            ));
        };
        let Some(host_id) = head.host_id.clone() else {
            return Err(error(
                ErrorCode::Internal,
                format!("the vault does not say which host has {session_id}"),
            ));
        };
        let Some(project_id) = head.project_id.clone() else {
            return Err(error(
                ErrorCode::Conflict,
                format!("the vault does not say which project {session_id} works on"),
            ));
        };
        Ok(Source {
            events: view.events,
            attachments: view.attachments,
            project_id: Some(project_id),
            host_id,
        })
    }
}

fn error(code: ErrorCode, message: String) -> ErrorInfo {
    ErrorInfo { code, message }
}
