//! Host-key verification decisions for each [`HostKeyPolicy`] variant. Pure
//! and network-free: everything here operates on an already-received server
//! public key and a `known_hosts` file location.

use std::path::Path;

use russh::keys::known_hosts::{learn_known_hosts, learn_known_hosts_path};
use russh::keys::{PublicKey, check_known_hosts, check_known_hosts_path};

use crate::config::HostKeyPolicy;
use crate::error::SshError;

/// Where to read and write `known_hosts` entries.
#[derive(Clone, Copy)]
enum Location<'a> {
    /// The invoking user's default `~/.ssh/known_hosts`, resolved by russh
    /// itself.
    Default,
    /// An explicit file, used for [`HostKeyPolicy::KnownHosts`] and for
    /// tests that must never touch the real default location.
    At(&'a Path),
}

/// Decides whether `server_key` is acceptable for `host:port` under
/// `policy`, recording it to `known_hosts` first if the policy calls for it.
pub(crate) fn decide(
    policy: &HostKeyPolicy,
    host: &str,
    port: u16,
    server_key: &PublicKey,
) -> Result<(), SshError> {
    match policy {
        HostKeyPolicy::KnownHosts(path) => {
            verify_strict(host, port, server_key, Location::At(path))
        }
        HostKeyPolicy::AcceptNew => accept_new(host, port, server_key, Location::Default),
        HostKeyPolicy::Prompt => Err(SshError::UnsupportedHostKeyPolicy { policy: "prompt" }),
    }
}

fn check(
    host: &str,
    port: u16,
    key: &PublicKey,
    location: Location<'_>,
) -> Result<bool, russh::keys::Error> {
    match location {
        Location::Default => check_known_hosts(host, port, key),
        Location::At(path) => check_known_hosts_path(host, port, key, path),
    }
}

fn learn(
    host: &str,
    port: u16,
    key: &PublicKey,
    location: Location<'_>,
) -> Result<(), russh::keys::Error> {
    match location {
        Location::Default => learn_known_hosts(host, port, key),
        Location::At(path) => learn_known_hosts_path(host, port, key, path),
    }
}

/// [`HostKeyPolicy::KnownHosts`]: accept only an exact match; reject an
/// unknown host or a changed key. Never writes to the file.
fn verify_strict(
    host: &str,
    port: u16,
    server_key: &PublicKey,
    location: Location<'_>,
) -> Result<(), SshError> {
    match check(host, port, server_key, location) {
        Ok(true) => Ok(()),
        Ok(false) => Err(SshError::HostKeyUnknown {
            host: host.to_owned(),
            port,
        }),
        Err(russh::keys::Error::KeyChanged { .. }) => Err(SshError::HostKeyChanged {
            host: host.to_owned(),
            port,
        }),
        Err(_) => Err(SshError::HostKeyStore {
            reason: "could not read the known_hosts file".to_owned(),
        }),
    }
}

/// [`HostKeyPolicy::AcceptNew`]: trust-on-first-use. An absent host is
/// accepted and recorded; a matching host is accepted without duplicating
/// the entry; a changed key is rejected, never silently accepted.
fn accept_new(
    host: &str,
    port: u16,
    server_key: &PublicKey,
    location: Location<'_>,
) -> Result<(), SshError> {
    match check(host, port, server_key, location) {
        Ok(true) => Ok(()),
        Ok(false) => {
            learn(host, port, server_key, location).map_err(|_err| SshError::HostKeyStore {
                reason: "could not record the ssh host key".to_owned(),
            })
        }
        Err(russh::keys::Error::KeyChanged { .. }) => Err(SshError::HostKeyChanged {
            host: host.to_owned(),
            port,
        }),
        Err(_) => Err(SshError::HostKeyStore {
            reason: "could not read the known_hosts file".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Location, accept_new, verify_strict};
    use crate::error::SshError;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn fixture_key(name: &str) -> russh::keys::PublicKey {
        russh::keys::load_public_key(fixture(name)).expect("fixture public key should parse")
    }

    #[test]
    fn known_hosts_accepts_a_matching_key() {
        let key = fixture_key("id_ed25519.pub");
        let path = fixture("known_hosts");
        assert!(verify_strict("match.example.test", 22, &key, Location::At(&path)).is_ok());
    }

    #[test]
    fn known_hosts_rejects_a_changed_key() {
        let key = fixture_key("id_ed25519.pub");
        let path = fixture("known_hosts");
        let err = verify_strict("changed.example.test", 22, &key, Location::At(&path)).unwrap_err();
        assert!(matches!(err, SshError::HostKeyChanged { .. }));
    }

    #[test]
    fn known_hosts_rejects_an_unknown_host() {
        let key = fixture_key("id_ed25519.pub");
        let path = fixture("known_hosts");
        let err = verify_strict("unknown.example.test", 22, &key, Location::At(&path)).unwrap_err();
        assert!(matches!(err, SshError::HostKeyUnknown { .. }));
    }

    #[test]
    fn accept_new_records_an_unknown_host_and_accepts_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let known_hosts_path = dir.path().join("known_hosts");
        let key = fixture_key("id_ed25519.pub");

        let result = accept_new(
            "fresh.example.test",
            22,
            &key,
            Location::At(&known_hosts_path),
        );
        assert!(result.is_ok());

        let contents = std::fs::read_to_string(&known_hosts_path)
            .expect("known_hosts should have been written");
        assert!(contents.contains("fresh.example.test"));
        assert!(contents.contains("ssh-ed25519"));
    }

    #[test]
    fn accept_new_accepts_a_matching_key_without_duplicating_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let known_hosts_path = dir.path().join("known_hosts");
        let key = fixture_key("id_ed25519.pub");

        accept_new(
            "stable.example.test",
            22,
            &key,
            Location::At(&known_hosts_path),
        )
        .expect("first accept should succeed");
        accept_new(
            "stable.example.test",
            22,
            &key,
            Location::At(&known_hosts_path),
        )
        .expect("second accept of the same key should still succeed");

        let contents = std::fs::read_to_string(&known_hosts_path).expect("file should exist");
        let occurrences = contents.matches("stable.example.test").count();
        assert_eq!(
            occurrences, 1,
            "a matching key must not duplicate the known_hosts entry"
        );
    }

    #[test]
    fn verify_strict_returns_host_key_store_when_the_known_hosts_path_is_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = fixture_key("id_ed25519.pub");

        // A directory in place of the known_hosts file opens but cannot be
        // read as a line-oriented file, which is a store failure distinct
        // from "host not present".
        let err = verify_strict(
            "unreadable.example.test",
            22,
            &key,
            Location::At(dir.path()),
        )
        .unwrap_err();
        assert!(matches!(err, SshError::HostKeyStore { .. }));
    }

    #[test]
    fn accept_new_returns_host_key_store_when_the_known_hosts_path_is_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = fixture_key("id_ed25519.pub");

        let err = accept_new(
            "unreadable.example.test",
            22,
            &key,
            Location::At(dir.path()),
        )
        .unwrap_err();
        assert!(matches!(err, SshError::HostKeyStore { .. }));
    }

    #[test]
    fn accept_new_returns_host_key_store_when_recording_the_key_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocking_file = dir.path().join("not_a_directory");
        std::fs::write(&blocking_file, b"").expect("blocking file should be created");
        // The known_hosts path's parent is an existing regular file, so the
        // host reads as unknown (no known_hosts to open) but recording the
        // key fails when the writer tries to create that "directory".
        let known_hosts_path = blocking_file.join("known_hosts");
        let key = fixture_key("id_ed25519.pub");

        let err = accept_new(
            "unwritable.example.test",
            22,
            &key,
            Location::At(&known_hosts_path),
        )
        .unwrap_err();
        assert!(matches!(err, SshError::HostKeyStore { .. }));
    }

    #[test]
    fn accept_new_rejects_a_changed_key_instead_of_silently_accepting_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let known_hosts_path = dir.path().join("known_hosts");
        let original = fixture_key("id_ed25519.pub");
        // Same algorithm, different key material: an algorithm mismatch
        // alone reads as "no match" to russh, not "changed" -- the fixture
        // must swap the key itself to exercise the KeyChanged path.
        let changed = fixture_key("id_ed25519_other.pub");

        accept_new(
            "flip.example.test",
            22,
            &original,
            Location::At(&known_hosts_path),
        )
        .expect("first accept should succeed");
        let err = accept_new(
            "flip.example.test",
            22,
            &changed,
            Location::At(&known_hosts_path),
        )
        .unwrap_err();
        assert!(matches!(err, SshError::HostKeyChanged { .. }));
    }
}
