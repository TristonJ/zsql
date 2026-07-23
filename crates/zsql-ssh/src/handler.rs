//! The russh client handler. Kept private: this is the only place in the
//! crate that names russh's `Handler` trait, and it dispatches host-key
//! handling through [`HostKeyPolicy`] so each policy variant controls its
//! own verification behavior.

use crate::config::HostKeyPolicy;

pub(crate) struct ClientHandler {
    host_key: HostKeyPolicy,
}

impl ClientHandler {
    pub(crate) fn new(host_key: HostKeyPolicy) -> Self {
        Self { host_key }
    }

    /// The policy-driven decision `check_server_key` reports to russh,
    /// split out so it can be unit tested without a real SSH handshake.
    fn accept_server_key(&self) -> bool {
        match self.host_key {
            HostKeyPolicy::AcceptNew => true,
            HostKeyPolicy::KnownHosts(_) | HostKeyPolicy::Prompt => false,
        }
    }
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(self.accept_server_key())
    }
}

#[cfg(test)]
mod tests {
    use super::ClientHandler;
    use crate::config::HostKeyPolicy;

    #[test]
    fn accept_new_policy_accepts_the_server_key() {
        let handler = ClientHandler::new(HostKeyPolicy::AcceptNew);
        assert!(handler.accept_server_key());
    }

    #[test]
    fn known_hosts_policy_does_not_accept_yet() {
        let handler = ClientHandler::new(HostKeyPolicy::KnownHosts("/dev/null".into()));
        assert!(!handler.accept_server_key());
    }

    #[test]
    fn prompt_policy_does_not_accept_yet() {
        let handler = ClientHandler::new(HostKeyPolicy::Prompt);
        assert!(!handler.accept_server_key());
    }
}
