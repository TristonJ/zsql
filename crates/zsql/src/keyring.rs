//! Wrapper around the `keyring` crate specifically for usage in zsql.
#[cfg(test)]
use std::{fs::File, io::Write};

/// A thin wrapper around a `keyring::Entry`. This is safe to use in unit tests, as it will
/// fallback to a simple file-based storage mechanism.
pub struct Entry {
    #[cfg(not(test))]
    entry: keyring::Entry,
    #[cfg(test)]
    entry_path: std::path::PathBuf,
}

/// The keyring "service" we're using - basically a namespace for our secrets
pub const KEYRING_SERVICE: &str = "zsql";

#[derive(Debug)]
pub struct Error {
    message: String,
    /// Whether this error means no credential is stored for the account
    /// accessed, as opposed to a keyring access failure (a locked keyring, a
    /// dbus/platform error, etc).
    absent: bool,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keyring error: {}", self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Whether this error means no credential is stored for the account
    /// accessed. `false` for any other keyring access failure (a locked
    /// keyring, a dbus/platform error, etc), which a caller should surface
    /// as an error rather than treat as "nothing saved yet".
    #[must_use]
    pub fn is_absent(&self) -> bool {
        self.absent
    }
}

impl From<keyring::Error> for Error {
    fn from(error: keyring::Error) -> Self {
        let absent = matches!(error, keyring::Error::NoEntry);
        Self {
            message: error.to_string(),
            absent,
        }
    }
}

#[cfg(test)]
impl Error {
    /// The error returned by the test-mode mock when a credential has never
    /// been set or has been deleted, mirroring `keyring::Error::NoEntry`
    /// (which cannot be constructed outside the `keyring` crate).
    fn missing() -> Self {
        Self {
            message: "no matching credential found".to_owned(),
            absent: true,
        }
    }

    /// A keyring access failure that is not "no credential stored", e.g. a
    /// locked keyring or a dbus/platform error.
    fn other(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            absent: false,
        }
    }
}

#[cfg(not(test))]
impl Entry {
    pub fn new(name: impl AsRef<str>) -> Result<Self, Error> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, name.as_ref())?;
        Ok(Self { entry })
    }

    pub fn set_password(&self, password: &str) -> Result<(), Error> {
        self.entry.set_password(password)?;
        Ok(())
    }

    pub fn get_password(&self) -> Result<String, Error> {
        let password = self.entry.get_password()?;
        Ok(password)
    }

    /// Delete the credential. Deleting an entry that does not exist is not
    /// an error: the end state either way is "no credential stored".
    pub fn delete(&self) -> Result<(), Error> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(test)]
impl Entry {
    // These mock methods always return `Ok` (or panic on unexpected I/O
    // failure), unlike their `#[cfg(not(test))]` counterparts, which are
    // genuinely fallible. The `Result` return types are kept anyway so
    // every call site works identically regardless of which impl is
    // compiled in.
    #[allow(clippy::unnecessary_wraps)]
    pub fn new(name: impl AsRef<str>) -> Result<Self, Error> {
        let path =
            std::env::temp_dir().join(format!("{}-unit-test-{}", KEYRING_SERVICE, name.as_ref()));
        Ok(Self { entry_path: path })
    }

    pub fn set_password(&self, password: &str) -> Result<(), Error> {
        if self.write_block_path().exists() {
            return Err(Error::other("mock keyring write blocked for test"));
        }
        let mut file = File::create(&self.entry_path).expect("failed to create mock keyring file");
        file.write_all(password.as_bytes())
            .expect("failed to write mock keyring file");
        Ok(())
    }

    /// The marker file [`Self::block_writes_for_test`] leaves next to this
    /// entry's own file, checked by [`Self::set_password`].
    fn write_block_path(&self) -> std::path::PathBuf {
        self.entry_path.with_extension("write-blocked")
    }

    /// Force this entry's [`Self::set_password`] to fail with a non-absent
    /// error, without affecting [`Self::get_password`], for tests
    /// exercising a keyring write failure distinct from an absent read.
    pub(crate) fn block_writes_for_test(&self) {
        std::fs::write(self.write_block_path(), b"")
            .expect("failed to write mock keyring block marker");
    }

    /// A missing file classifies as [`Error::missing`] ("no credential
    /// stored"); any other read failure classifies as [`Error::other`], so a
    /// test can simulate a locked-keyring-style failure via
    /// [`Self::corrupt_for_test`] without a real OS keyring.
    pub fn get_password(&self) -> Result<String, Error> {
        match std::fs::read_to_string(&self.entry_path) {
            Ok(password) => Ok(password),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(Error::missing()),
            Err(err) => Err(Error::other(err.to_string())),
        }
    }

    /// Delete the mock credential file. Deleting an entry that does not
    /// exist is not an error: the end state either way is "no credential
    /// stored", mirroring the real keyring's `NoEntry`-tolerant delete.
    #[allow(clippy::unnecessary_wraps)]
    pub fn delete(&self) -> Result<(), Error> {
        match std::fs::remove_file(&self.entry_path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => panic!("failed to delete mock keyring file: {err}"),
        }
    }

    /// Force this entry into a state where [`Self::get_password`] fails with
    /// a non-absent [`Error`], for tests exercising a keyring access failure
    /// distinctly from "no credential stored".
    pub(crate) fn corrupt_for_test(&self) {
        let _ = std::fs::remove_file(&self.entry_path);
        std::fs::create_dir_all(&self.entry_path)
            .expect("failed to set up a corrupted mock keyring entry");
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Error};

    /// A unique mock entry name per test, so parallel test runs never
    /// collide on the same file in the OS temp directory.
    fn unique_name(label: &str) -> String {
        format!("classify-test-{label}-{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn a_url_that_was_never_set_is_classified_as_absent() {
        let entry =
            Entry::new(unique_name("never-set")).expect("mock entry construction cannot fail");
        let err = entry
            .get_password()
            .expect_err("nothing was ever set for this entry");
        assert!(err.is_absent());
    }

    #[test]
    fn a_deleted_credential_is_classified_as_absent() {
        let entry =
            Entry::new(unique_name("deleted")).expect("mock entry construction cannot fail");
        entry
            .set_password("secret")
            .expect("set_password must succeed");
        entry.delete().expect("delete must succeed");

        let err = entry
            .get_password()
            .expect_err("the credential was deleted");
        assert!(err.is_absent());
    }

    #[test]
    fn a_corrupted_entry_is_classified_as_not_absent() {
        let name = unique_name("corrupted");
        let entry = Entry::new(&name).expect("mock entry construction cannot fail");
        entry.corrupt_for_test();

        let err = entry
            .get_password()
            .expect_err("a corrupted entry must fail to read");
        assert!(
            !err.is_absent(),
            "a keyring access failure must not be classified the same as an absent credential"
        );

        let _ = std::fs::remove_dir_all(
            std::env::temp_dir().join(format!("{}-unit-test-{name}", super::KEYRING_SERVICE)),
        );
    }

    #[test]
    fn the_real_backends_no_entry_error_classifies_as_absent() {
        let err: Error = keyring::Error::NoEntry.into();
        assert!(err.is_absent());
    }

    #[test]
    fn the_real_backends_invalid_error_classifies_as_not_absent() {
        let err: Error =
            keyring::Error::Invalid("account".to_owned(), "too long".to_owned()).into();
        assert!(
            !err.is_absent(),
            "a keyring access failure must not be classified the same as an absent credential"
        );
    }
}
