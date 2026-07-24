//! The russh client handler. Kept private: this is the only place in the
//! crate that names russh's `Handler` trait, and it dispatches host-key
//! handling through [`crate::host_key`] so each policy variant controls its
//! own verification behavior.

use std::sync::{Arc, Mutex};

use crate::config::HostKeyPolicy;
use crate::error::SshError;
use crate::host_key;

pub(crate) struct ClientHandler {
    host_key: HostKeyPolicy,
    host: String,
    port: u16,
    host_key_error: Arc<Mutex<Option<SshError>>>,
}

impl ClientHandler {
    /// Builds the handler alongside a shared cell that captures the exact
    /// host-key rejection reason. russh's handshake only reports a generic
    /// failure once `check_server_key` returns `false`, so the caller
    /// recovers the precise `SshError` from this cell after `connect`
    /// fails, instead of losing it to a generic connect error.
    pub(crate) fn new(
        host_key: HostKeyPolicy,
        host: String,
        port: u16,
    ) -> (Self, Arc<Mutex<Option<SshError>>>) {
        let host_key_error = Arc::new(Mutex::new(None));
        let handler = Self {
            host_key,
            host,
            port,
            host_key_error: Arc::clone(&host_key_error),
        };
        (handler, host_key_error)
    }
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let span = tracing::info_span!("ssh_check_server_key", host = %self.host, port = self.port);
        let _enter = span.enter();

        match host_key::decide(&self.host_key, &self.host, self.port, server_public_key) {
            Ok(()) => {
                tracing::info!("ssh host key accepted");
                Ok(true)
            }
            Err(err) => {
                tracing::warn!(error = %err, "ssh host key rejected");
                if let Ok(mut slot) = self.host_key_error.lock() {
                    *slot = Some(err);
                }
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use russh::client::Handler as _;

    use super::ClientHandler;
    use crate::config::HostKeyPolicy;
    use crate::error::SshError;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn fixture_key(name: &str) -> russh::keys::PublicKey {
        russh::keys::load_public_key(fixture(name)).expect("fixture public key should parse")
    }

    #[tokio::test]
    async fn known_hosts_policy_accepts_a_matching_recorded_key() {
        let (mut handler, host_key_error) = ClientHandler::new(
            HostKeyPolicy::KnownHosts(fixture("known_hosts")),
            "match.example.test".to_owned(),
            22,
        );
        let key = fixture_key("id_ed25519.pub");

        let accepted = handler.check_server_key(&key).await.unwrap();

        assert!(accepted);
        assert!(host_key_error.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn known_hosts_policy_rejects_an_unrecorded_host_and_records_why() {
        let (mut handler, host_key_error) = ClientHandler::new(
            HostKeyPolicy::KnownHosts(fixture("known_hosts")),
            "unknown.example.test".to_owned(),
            22,
        );
        let key = fixture_key("id_ed25519.pub");

        let accepted = handler.check_server_key(&key).await.unwrap();

        assert!(!accepted);
        assert!(matches!(
            host_key_error.lock().unwrap().take(),
            Some(SshError::HostKeyUnknown { .. })
        ));
    }

    #[tokio::test]
    async fn known_hosts_policy_rejects_a_changed_key_and_records_why() {
        let (mut handler, host_key_error) = ClientHandler::new(
            HostKeyPolicy::KnownHosts(fixture("known_hosts")),
            "changed.example.test".to_owned(),
            22,
        );
        let key = fixture_key("id_ed25519.pub");

        let accepted = handler.check_server_key(&key).await.unwrap();

        assert!(!accepted);
        assert!(matches!(
            host_key_error.lock().unwrap().take(),
            Some(SshError::HostKeyChanged { .. })
        ));
    }

    #[tokio::test]
    async fn prompt_policy_rejects_and_records_the_unsupported_error() {
        let (mut handler, host_key_error) =
            ClientHandler::new(HostKeyPolicy::Prompt, "db.example.com".to_owned(), 22);
        let key = fixture_key("id_ed25519.pub");

        let accepted = handler.check_server_key(&key).await.unwrap();

        assert!(!accepted);
        assert!(matches!(
            host_key_error.lock().unwrap().take(),
            Some(SshError::UnsupportedHostKeyPolicy { policy: "prompt" })
        ));
    }
}
