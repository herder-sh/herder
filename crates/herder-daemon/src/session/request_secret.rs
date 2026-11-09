//! `request_secret`: an agent asks the user to supply a secret it does not have.
//!
//! It raises an approval that is always the user's ([`actor::SecretAsk::Request`]). An owner
//! answers it from an app: they type the value there, the app puts it on the host's vault as
//! that owner with `put_secret`, and says so with `confirm_secret_stored`. The value never
//! passes through this host, so neither the approval, nor the journal, nor the tool's result,
//! nor any log can carry it. A denial, or the turn ending first, stores nothing.

use herder_protocol::{SecretInfo, SecretRequest, SessionId};
use herder_tasktools::{RequestSecretInput, RequestSecretOutcome, RequestSecretOutput, ToolError};

use super::SessionManager;
use super::actor;
use super::store_secret::check;

impl SessionManager {
    /// `request_secret` of `caller`: asks the user, and answers once an owner stored the value
    /// or the user declined.
    pub(super) async fn request_secret(
        &self,
        caller: SessionId,
        input: RequestSecretInput,
    ) -> Result<RequestSecretOutput, ToolError> {
        self.check_linked()?;
        check(&input.set, &input.name, &input.description)?;
        // Without the vault's list now, the request only lacks its warning.
        let replaces = match self.inner.secrets.get() {
            Some(secrets) => secrets
                .list()
                .await
                .is_ok_and(|listed| holds(&listed, &input.set, &input.name)),
            None => false,
        };
        let request = SecretRequest {
            set: input.set.clone(),
            name: input.name.clone(),
            description: input.description,
            replaces,
        };
        let stored = self
            .ask_owner(caller, actor::SecretAsk::Request(request))
            .await?;
        Ok(match stored {
            Some(set) => RequestSecretOutput {
                outcome: RequestSecretOutcome::Stored,
                set,
                name: input.name,
            },
            None => RequestSecretOutput {
                outcome: RequestSecretOutcome::Declined,
                set: input.set,
                name: input.name,
            },
        })
    }
}

/// Whether `listed`, the secrets of the sets granted to this host, has `name` in `set`.
fn holds(listed: &[SecretInfo], set: &str, name: &str) -> bool {
    listed
        .iter()
        .any(|secret| secret.set == set && secret.name == name)
}

#[cfg(test)]
mod tests {
    use herder_protocol::Timestamp;

    use super::*;

    #[test]
    fn a_request_replaces_only_the_same_name_in_the_same_set() {
        let secret = |set: &str, name: &str| SecretInfo {
            set: set.into(),
            name: name.into(),
            description: String::new(),
            updated_at: Timestamp::now(),
        };
        let listed = [
            secret("coingate", "GITHUB_SIEM_PAT"),
            secret("personal", "STRIPE_KEY"),
        ];
        assert!(holds(&listed, "coingate", "GITHUB_SIEM_PAT"));
        assert!(!holds(&listed, "personal", "GITHUB_SIEM_PAT"));
        assert!(!holds(&listed, "coingate", "STRIPE_KEY"));
        assert!(!holds(&[], "coingate", "GITHUB_SIEM_PAT"));
    }
}
