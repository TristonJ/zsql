//! ssh-agent authentication ([`crate::config::SshAuth::Agent`]): connects to
//! the agent named by `SSH_AUTH_SOCK` and tries each identity it offers
//! until the server accepts one.

use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PublicKey};
use tokio::net::UnixStream;

use crate::error::SshError;
use crate::tunnel::SessionHandle;

/// Fails fast if no ssh-agent is reachable, without attempting to sign
/// anything. Used as a preflight so an unreachable agent is reported before
/// the SSH host is even dialed, rather than surfacing deep inside a stalled
/// authentication round trip.
pub(crate) async fn ensure_available() -> Result<(), SshError> {
    connect().await.map(drop)
}

/// Enumerates the agent's identities and tries `authenticate_publickey_with`
/// against each public-key identity until the server accepts one. Returns
/// `Ok(false)` (not an error) if the agent is reachable but every identity
/// it offered was rejected, so the caller reports a normal auth failure
/// rather than a connectivity problem.
pub(crate) async fn authenticate(
    session: &mut SessionHandle,
    user: &str,
) -> Result<bool, SshError> {
    let mut agent = connect().await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|_err| SshError::AgentUnavailable)?;

    for identity in identities {
        let AgentIdentity::PublicKey {
            key: public_key, ..
        } = identity
        else {
            continue; // certificate identities are not supported via authenticate_publickey_with
        };

        let hash_alg = rsa_hash_alg(session, &public_key).await;

        let result = session
            .authenticate_publickey_with(user, public_key, hash_alg, &mut agent)
            .await;
        if matches!(result, Ok(auth_result) if auth_result.success()) {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn rsa_hash_alg(session: &mut SessionHandle, key: &PublicKey) -> Option<HashAlg> {
    if key.algorithm().is_rsa() {
        session
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten()
    } else {
        None
    }
}

async fn connect() -> Result<AgentClient<UnixStream>, SshError> {
    connect_at(std::env::var("SSH_AUTH_SOCK").ok().as_deref()).await
}

/// The connect step split out from [`connect`] so tests can force the
/// "no agent" and "agent socket missing" paths deterministically without
/// touching the process's real `SSH_AUTH_SOCK` (this is also exactly what
/// [`ensure_available`] and [`authenticate`] call in production).
async fn connect_at(sock_path: Option<&str>) -> Result<AgentClient<UnixStream>, SshError> {
    let Some(path) = sock_path else {
        return Err(SshError::AgentUnavailable);
    };
    AgentClient::connect_uds(path)
        .await
        .map_err(|_err| SshError::AgentUnavailable)
}

#[cfg(test)]
mod tests {
    use super::connect_at;
    use crate::error::SshError;

    #[tokio::test]
    async fn connect_fails_fast_when_no_sock_path_is_given() {
        let result = connect_at(None).await;
        assert!(matches!(result, Err(SshError::AgentUnavailable)));
    }

    #[tokio::test]
    async fn connect_fails_fast_when_the_sock_path_does_not_exist() {
        let result = connect_at(Some("/nonexistent/zsql-ssh-test-agent.sock")).await;
        assert!(matches!(result, Err(SshError::AgentUnavailable)));
    }
}
