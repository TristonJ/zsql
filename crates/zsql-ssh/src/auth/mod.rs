//! Authentication strategies for [`SshAuth`]. Each variant maps to a
//! submodule; this module dispatches to the right one and folds "the server
//! didn't accept it" into the crate's single [`SshError::AuthFailed`] shape
//! regardless of which method was tried.

mod agent;
mod key;

pub(crate) use agent::ensure_available as ensure_agent_available;

use crate::config::SshAuth;
use crate::error::SshError;
use crate::tunnel::SessionHandle;

pub(crate) async fn authenticate(
    session: &mut SessionHandle,
    user: &str,
    auth: &SshAuth,
) -> Result<(), SshError> {
    let span = tracing::info_span!("ssh_authenticate", user, method = auth_method_name(auth));
    let _enter = span.enter();

    let success = match auth {
        SshAuth::Password(password) => session
            .authenticate_password(user, password.as_str())
            .await
            .map_err(|err| SshError::Session {
                reason: err.to_string(),
            })?
            .success(),
        SshAuth::Key { path, passphrase } => {
            key::authenticate(session, user, path, passphrase.as_deref()).await?
        }
        SshAuth::Agent => agent::authenticate(session, user).await?,
    };

    if success {
        tracing::info!("ssh authentication succeeded");
    } else {
        tracing::warn!("ssh authentication failed");
    }
    require_success(user, success)
}

fn require_success(user: &str, success: bool) -> Result<(), SshError> {
    if success {
        Ok(())
    } else {
        Err(SshError::AuthFailed {
            user: user.to_owned(),
        })
    }
}

fn auth_method_name(auth: &SshAuth) -> &'static str {
    match auth {
        SshAuth::Password(_) => "password",
        SshAuth::Key { .. } => "key",
        SshAuth::Agent => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::{auth_method_name, require_success};
    use crate::config::SshAuth;
    use crate::error::SshError;

    #[test]
    fn require_success_accepts_a_successful_result() {
        assert!(require_success("alice", true).is_ok());
    }

    #[test]
    fn require_success_reports_auth_failed_for_an_unsuccessful_result() {
        let err = require_success("alice", false).unwrap_err();
        match err {
            SshError::AuthFailed { user } => assert_eq!(user, "alice"),
            other => panic!("expected SshError::AuthFailed, got {other:?}"),
        }
    }

    #[test]
    fn auth_method_name_reports_each_auth_kind() {
        assert_eq!(auth_method_name(&SshAuth::Password("x".into())), "password");
        assert_eq!(auth_method_name(&SshAuth::Agent), "agent");
        assert_eq!(
            auth_method_name(&SshAuth::Key {
                path: "/x".into(),
                passphrase: None,
            }),
            "key"
        );
    }
}
