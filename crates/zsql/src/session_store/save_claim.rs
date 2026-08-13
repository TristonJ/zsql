//! Ordering guarantee for concurrent writes to the same session directory:
//! a caller mints a [`SaveClaim`] synchronously, in call order, before
//! handing a write off to a background executor.
//!
//! Every directory's minted counter and last-written state live in state
//! shared between the factory and every claim it mints, taken together
//! with [`super::session_dir::IO_LOCK`] for the duration of an actual write

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::SessionStoreError;
use super::session_dir::IoGuard;

/// Per-directory minted counter and last-written state.
#[derive(Default)]
struct DirState {
    minted: u64,
    written: u64,
}

/// The state a [`SaveClaimFactory`] and every [`SaveClaim`] it
/// mints share.
type SharedState = Arc<Mutex<HashMap<PathBuf, DirState>>>;

/// Mints [`SaveClaim`]s and owns the per-directory state they are checked
/// against
#[derive(Clone, Default)]
pub struct SaveClaimFactory {
    state: SharedState,
}

impl SaveClaimFactory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint the next claim for `dir`, starting at 1
    #[tracing::instrument(name = "save_claim_mint", skip(self), fields(dir = %dir.display()))]
    pub fn mint(&self, dir: &Path) -> SaveClaim {
        let mut state = lock(&self.state);
        let entry = state.entry(dir.to_path_buf()).or_default();
        entry.minted += 1;
        let value = entry.minted;
        drop(state);
        SaveClaim {
            state: Arc::clone(&self.state),
            dir: dir.to_path_buf(),
            value,
        }
    }
}

/// One claim on a position in a directory's save order, minted by
/// [`SaveClaimFactory::mint`]
pub struct SaveClaim {
    state: SharedState,
    dir: PathBuf,
    value: u64,
}

impl std::fmt::Debug for SaveClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveClaim")
            .field("dir", &self.dir)
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

impl SaveClaim {
    /// The directory this claim was minted for
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This claim's position in its directory's mint sequence, for logging.
    #[must_use]
    pub fn value(&self) -> u64 {
        self.value
    }

    /// Run `write` if this claim is still at least as new as the last
    /// claim recorded for its directory, returning whether it ran. Opens
    /// the same critical section [`super::session_dir::SessionDir`]'s
    /// own I/O methods use and hands its [`IoGuard`] to `write`, which
    /// spends it on a locked view over whatever it is about to write.
    ///
    /// # Errors
    /// Returns whatever `write` itself returns, if it runs.
    #[tracing::instrument(
        name = "save_claim_write_if_current",
        skip(self, write),
        fields(dir = %self.dir.display(), claim = self.value)
    )]
    pub fn write_if_current(
        self,
        write: impl FnOnce(IoGuard) -> Result<(), SessionStoreError>,
    ) -> Result<bool, SessionStoreError> {
        let guard = IoGuard::acquire();
        let mut state = lock(&self.state);
        let entry = state.entry(self.dir.clone()).or_default();
        if self.value < entry.written {
            tracing::info!(
                claim = self.value,
                latest = entry.written,
                "skipping stale write: a newer claim already landed"
            );
            return Ok(false);
        }
        entry.written = self.value;
        drop(state);
        write(guard)?;
        Ok(true)
    }

    /// Unconditionally raise this claim's directory state to at least
    /// its own position, without performing any write itself
    pub fn record_written(self) {
        let mut state = lock(&self.state);
        let entry = state.entry(self.dir).or_default();
        entry.written = entry.written.max(self.value);
    }
}

fn lock(state: &SharedState) -> std::sync::MutexGuard<'_, HashMap<PathBuf, DirState>> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::SaveClaimFactory;

    #[allow(clippy::unnecessary_wraps)]
    fn write_ok(_guard: super::IoGuard) -> Result<(), super::SessionStoreError> {
        Ok(())
    }

    #[test]
    fn the_first_claim_for_a_directory_is_minted_at_one_and_writes() {
        let factory = SaveClaimFactory::new();
        let claim = factory.mint(Path::new("/tmp/session-a"));

        assert_eq!(claim.value(), 1);
        assert!(
            claim.write_if_current(write_ok).expect("must not error"),
            "the first claim for a directory must write"
        );
    }

    #[test]
    fn an_older_claim_is_refused_after_a_newer_one_lands() {
        let factory = SaveClaimFactory::new();
        let dir = Path::new("/tmp/session-a");
        let older = factory.mint(dir);
        let newer = factory.mint(dir);

        assert!(newer.write_if_current(write_ok).expect("must not error"));
        let mut ran = false;
        let wrote = older
            .write_if_current(|_guard| {
                ran = true;
                Ok(())
            })
            .expect("a refused write must not error");
        assert!(!wrote, "the older claim must be refused");
        assert!(!ran, "a refused claim must never run its write");
    }

    #[test]
    fn record_written_fences_an_older_claim_without_writing_anything() {
        let factory = SaveClaimFactory::new();
        let dir = Path::new("/tmp/session-a");
        let older = factory.mint(dir);
        let fence = factory.mint(dir);

        fence.record_written();

        assert!(
            !older.write_if_current(write_ok).expect("must not error"),
            "a claim below a recorded fence must be refused"
        );
    }

    #[test]
    fn claims_for_different_directories_are_ordered_independently() {
        let factory = SaveClaimFactory::new();
        let claim_a = factory.mint(Path::new("/tmp/session-a"));
        let claim_b = factory.mint(Path::new("/tmp/session-b"));

        assert_eq!(claim_a.value(), 1);
        assert_eq!(claim_b.value(), 1, "each directory mints its own sequence");
        assert!(claim_b.write_if_current(write_ok).expect("must not error"));
        assert!(
            claim_a.write_if_current(write_ok).expect("must not error"),
            "a write in one directory must never fence another directory's claim"
        );
    }

    #[test]
    fn factory_clones_mint_from_the_same_sequence() {
        let factory = SaveClaimFactory::new();
        let clone = factory.clone();
        let dir = Path::new("/tmp/session-a");

        let first = factory.mint(dir);
        let second = clone.mint(dir);

        assert_eq!(first.value(), 1);
        assert_eq!(
            second.value(),
            2,
            "a clone must continue the original's sequence, not start its own"
        );
        assert!(second.write_if_current(write_ok).expect("must not error"));
        assert!(
            !first.write_if_current(write_ok).expect("must not error"),
            "a claim minted through the original must be fenced by one spent through the clone"
        );
    }

    #[test]
    fn a_failed_write_still_raises_the_watermark_for_its_claim() {
        let factory = SaveClaimFactory::new();
        let dir = Path::new("/tmp/session-a");
        let older = factory.mint(dir);
        let newer = factory.mint(dir);

        let result = newer
            .write_if_current(|_guard| Err(super::SessionStoreError::UnsafeRef("boom".to_owned())));
        assert!(result.is_err(), "the write's own error must propagate");

        assert!(
            !older.write_if_current(write_ok).expect("must not error"),
            "even a failed newer write must fence older claims: its snapshot \
             was still the newer intent, and letting the older one land would \
             revert past it"
        );
    }
}
