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
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keyring error: {}", self.message)
    }
}

impl std::error::Error for Error {}

impl From<keyring::Error> for Error {
    fn from(error: keyring::Error) -> Self {
        Self {
            message: error.to_string(),
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

    // Mock method: always returns `Ok` (see the block comment above).
    #[allow(clippy::unnecessary_wraps)]
    pub fn set_password(&self, password: &str) -> Result<(), Error> {
        let mut file = File::create(&self.entry_path).expect("failed to create mock keyring file");
        file.write_all(password.as_bytes())
            .expect("failed to write mock keyring file");
        Ok(())
    }

    pub fn get_password(&self) -> Result<String, Error> {
        match std::fs::read_to_string(&self.entry_path) {
            Ok(password) => Ok(password),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(Error::missing()),
            Err(err) => panic!("failed to read mock keyring file: {err}"),
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
}
