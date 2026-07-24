//! Private-key-file authentication ([`crate::config::SshAuth::Key`]): loads
//! and, if necessary, decrypts the key file, then authenticates with it.

use std::path::Path;
use std::sync::Arc;

use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};

use crate::error::SshError;
use crate::tunnel::SessionHandle;

/// Loads the key at `path`, then authenticates `user` against `session`
/// with it. RSA keys negotiate the server's best supported signature hash
/// first; other key types have no such choice to make.
pub(crate) async fn authenticate(
    session: &mut SessionHandle,
    user: &str,
    path: &Path,
    passphrase: Option<&str>,
) -> Result<bool, SshError> {
    let span = tracing::info_span!("ssh_authenticate_key", user, path = %path.display());
    let _enter = span.enter();

    let key = Arc::new(load_key(path, passphrase)?);

    let hash_alg = if key.algorithm().is_rsa() {
        session
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten()
    } else {
        None
    };

    let result = session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash_alg))
        .await
        .map_err(|err| SshError::Session {
            reason: err.to_string(),
        })?;

    Ok(result.success())
}

fn load_key(path: &Path, passphrase: Option<&str>) -> Result<russh::keys::PrivateKey, SshError> {
    load_secret_key(path, passphrase).map_err(|err| classify_key_error(&err, path))
}

/// Distinguishes a passphrase problem from every other way a key file can
/// fail to load (missing, unreadable, corrupt, unsupported format), since
/// only the former asks the user for different input rather than a
/// different file.
fn classify_key_error(err: &russh::keys::Error, path: &Path) -> SshError {
    if is_passphrase_error(err) {
        SshError::KeyPassphrase {
            path: path.to_owned(),
        }
    } else {
        SshError::KeyUnreadable {
            path: path.to_owned(),
        }
    }
}

fn is_passphrase_error(err: &russh::keys::Error) -> bool {
    match err {
        russh::keys::Error::KeyIsEncrypted => true,
        russh::keys::Error::SshKey(inner) => matches!(
            inner,
            russh::keys::ssh_key::Error::Crypto
                | russh::keys::ssh_key::Error::Decrypted
                | russh::keys::ssh_key::Error::Encrypted
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{classify_key_error, load_key};
    use crate::error::SshError;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn loads_a_passphrase_free_ed25519_key() {
        let key = load_key(&fixture("id_ed25519"), None);
        assert!(key.is_ok(), "expected the fixture key to load: {key:?}");
    }

    #[test]
    fn loads_a_passphrase_free_rsa_key() {
        let key = load_key(&fixture("id_rsa"), None);
        assert!(key.is_ok(), "expected the fixture rsa key to load: {key:?}");
        assert!(key.unwrap().algorithm().is_rsa());
    }

    #[test]
    fn loads_an_encrypted_key_with_the_correct_passphrase() {
        let key = load_key(
            &fixture("id_ed25519_passphrase"),
            Some("fixture-passphrase"),
        );
        assert!(
            key.is_ok(),
            "expected the encrypted fixture key to load: {key:?}"
        );
    }

    #[test]
    fn missing_key_file_is_reported_as_unreadable_not_unsupported() {
        let err = load_key(&fixture("does-not-exist"), None).unwrap_err();
        assert!(matches!(err, SshError::KeyUnreadable { .. }));
    }

    #[test]
    fn encrypted_key_without_a_passphrase_is_reported_distinctly() {
        let err = load_key(&fixture("id_ed25519_passphrase"), None).unwrap_err();
        assert!(matches!(err, SshError::KeyPassphrase { .. }));
    }

    #[test]
    fn encrypted_key_with_the_wrong_passphrase_is_reported_distinctly() {
        let err =
            load_key(&fixture("id_ed25519_passphrase"), Some("not-the-right-one")).unwrap_err();
        assert!(matches!(err, SshError::KeyPassphrase { .. }));
    }

    #[test]
    fn classify_key_error_does_not_leak_the_underlying_russh_message() {
        // The io::Error string for "not found" varies by platform but never
        // needs to reach the user -- classify_key_error must not surface it.
        let io_err = std::io::Error::from(std::io::ErrorKind::NotFound);
        let err = classify_key_error(&russh::keys::Error::IO(io_err), &fixture("id_ed25519"));
        let rendered = err.to_string();
        assert!(!rendered.to_lowercase().contains("os error"));
    }
}
