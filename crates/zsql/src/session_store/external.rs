//! Reading and atomically writing an externally-owned script file: one
//! opened via Browse files..., living outside any session or library
//! directory. Shares [`super::session_dir::atomic_write`] and
//! its save lock.

use std::fs;
use std::path::Path;

use super::SessionStoreError;
use super::session_dir::IoGuard;
use super::session_dir::atomic_write;

/// Read `path`'s content, or `None` if it does not exist (moved or deleted
/// since it was opened).
///
/// # Errors
/// Returns [`SessionStoreError::Read`] if `path` exists but cannot be read.
#[tracing::instrument(name = "external_load", skip(path), fields(path = %path.display()))]
pub fn load(path: &Path) -> Result<Option<String>, SessionStoreError> {
    let _guard = IoGuard::acquire();
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(
        fs::read_to_string(path).map_err(SessionStoreError::Read)?,
    ))
}

/// Write `text` to `path`, creating its parent directory if needed.
///
/// # Errors
/// Returns [`SessionStoreError`] if the parent directory cannot be created
/// or the file cannot be written.
#[tracing::instrument(name = "external_save", skip(text), fields(path = %path.display()))]
pub fn save(path: &Path, text: &str) -> Result<(), SessionStoreError> {
    let _guard = IoGuard::acquire();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(SessionStoreError::Write)?;
    }
    atomic_write(path, text)?;
    tracing::info!(path = %path.display(), "external script saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, save};

    /// A temp directory this test owns exclusively, removed on drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-external-test-{label}-{}-{n}",
                std::process::id()
            ));
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saving_then_loading_reproduces_the_files_text_exactly() {
        let temp = TempDir::new("round-trip");
        let path = temp.0.join("migrate.sql");
        save(&path, "select * from orders;").expect("save must succeed");

        let loaded = load(&path).expect("load must succeed");

        assert_eq!(loaded, Some("select * from orders;".to_owned()));
    }

    #[test]
    fn loading_a_missing_file_returns_none_not_an_error() {
        let temp = TempDir::new("missing");
        let loaded = load(&temp.0.join("gone.sql")).expect("load must succeed");
        assert_eq!(loaded, None);
    }

    #[test]
    fn saving_creates_the_files_parent_directory_if_it_does_not_exist_yet() {
        let temp = TempDir::new("create-dir");
        let path = temp.0.join("nested").join("script.sql");
        assert!(!temp.0.exists());
        save(&path, "select 1;").expect("save must succeed");
        assert!(path.is_file());
    }

    #[test]
    fn saving_twice_overwrites_rather_than_appends() {
        let temp = TempDir::new("overwrite");
        let path = temp.0.join("script.sql");
        save(&path, "select 1;").expect("first save must succeed");
        save(&path, "select 2;").expect("second save must succeed");

        assert_eq!(
            load(&path).expect("load must succeed"),
            Some("select 2;".to_owned())
        );
    }
}
