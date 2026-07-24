//! Wrapper around the `keyring` crate specifically for usage in zsql.
#[cfg(test)]
use std::{fs::File, io::Write};

/// A thin wrapper around a keyring::Entry. This is safe to use in unit tests, as it will
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
    error: keyring::Error,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keyring error: {}", self.error)
    }
}

impl std::error::Error for Error {}

impl From<keyring::Error> for Error {
    fn from(error: keyring::Error) -> Self {
        Self { error }
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

    pub fn delete(&self) -> Result<(), Error> {
        self.entry.delete_credential()?;
        Ok(())
    }
}

#[cfg(test)]
impl Entry {
    pub fn new(name: impl AsRef<str>) -> Result<Self, Error> {
        let path =
            std::env::temp_dir().join(format!("{}-unit-test-{}", KEYRING_SERVICE, name.as_ref()));
        Ok(Self { entry_path: path })
    }

    pub fn set_password(&self, password: &str) -> Result<(), Error> {
        let mut file = File::create(&self.entry_path).expect("failed to create mock keyring file");
        file.write_all(password.as_bytes())
            .expect("failed to write mock keyring file");
        Ok(())
    }

    pub fn get_password(&self) -> Result<String, Error> {
        let password =
            std::fs::read_to_string(&self.entry_path).expect("failed to read mock keyring file");
        Ok(password)
    }

    pub fn delete(&self) -> Result<(), Error> {
        std::fs::remove_file(&self.entry_path).expect("failed to delete mock keyring file");
        Ok(())
    }
}
