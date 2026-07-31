//! A connection's tab-session snapshot: the ordered tabs, their buffers, and
//! which one was active, persisted under
//! [`crate::config::Config::sessions_dir`] in a per-connection directory so a
//! reconnect (or app restart) can rebuild the same tabs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

pub mod backing;
mod disk;
pub mod external;
pub mod library;
pub mod migration;
mod save_claim;
mod session_dir;
mod snapshot;
#[cfg(test)]
mod tests;

pub use backing::{LibraryName, ScriptBacking, ScriptFileName, SessionIo};
pub(crate) use disk::{
    draft_file_name, is_unsafe_script_name, script_file_name, unique_script_file_name,
};
pub use library::LibraryDir;
pub use save_claim::{SaveClaim, SaveClaimFactory};
pub(crate) use session_dir::SCRIPTS_DIR_NAME;
pub use session_dir::SessionDir;
pub use snapshot::{TabEntrySnapshot, TabKind, TabSessionSnapshot};

use crate::ui::connections::ActiveConnection;

/// Extension every script's file (a session sibling or a library file)
/// carries
pub const SCRIPT_FILE_EXTENSION: &str = ".sql";

/// Prefix a `Script` entry's `file` ref carries in `tabs.toml` when it names
/// a library file rather than a session-dir sibling: e.g.
/// `library:revenue-report.sql`.
pub(crate) const LIBRARY_FILE_PREFIX: &str = "library:";

/// Errors loading or saving a connection's session directory, the shared
/// library, or an external file.
#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    /// A session file exists but could not be read.
    #[error("failed to read session store: {0}")]
    Read(std::io::Error),
    /// A session file's contents could not be parsed as this shape.
    #[error("failed to parse session store: {0}")]
    Parse(#[from] toml::de::Error),
    /// An in-memory value could not be serialized to this shape.
    #[error("failed to serialize session store: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// A session file could not be written.
    #[error("failed to write session store: {0}")]
    Write(std::io::Error),
    /// A `tabs.toml` entry's file or draft ref resolves outside its session
    /// directory once joined and normalized, or names an invalid script or
    /// library identity.
    #[error("session entry ref escapes its session directory: {0}")]
    UnsafeRef(String),
    /// A rename or save target already exists
    #[error("{0} already exists")]
    Duplicate(String),
}

/// The stable identity a connection's tab-session snapshot is keyed under
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionKey {
    /// A saved connection's stable id
    /// (`crate::connections::StoredConnection::id`)
    Saved(Uuid),
    /// A connection reached only through a URL fallback (e.g.
    /// `DATABASE_URL`) with no saved entry behind it. Every such connection
    /// shares this one key
    Unsaved,
}

/// Directory name reserved for [`ConnectionKey::Unsaved`]'s session
/// directory
const UNSAVED_SESSION_DIR_NAME: &str = "unsaved";

impl ConnectionKey {
    fn storage_dir_name(self) -> String {
        match self {
            Self::Saved(id) => id.to_string(),
            Self::Unsaved => UNSAVED_SESSION_DIR_NAME.to_owned(),
        }
    }
}

/// A workspace's tab-session persistence state
pub struct SessionStore {
    root: Option<PathBuf>,
    active_key: Option<ConnectionKey>,
    last_active_connection: Option<ActiveConnection>,
    suppress_next_save: bool,
    session_cache: HashMap<ConnectionKey, Arc<TabSessionSnapshot>>,
    claims: SaveClaimFactory,
}

impl SessionStore {
    /// A store rooted at `root` (typically
    /// [`crate::config::Config::sessions_dir`]), or one that persists
    /// nothing if `root` is `None` (e.g. no data directory could be
    /// resolved at startup).
    #[must_use]
    pub fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            active_key: None,
            last_active_connection: None,
            suppress_next_save: false,
            session_cache: HashMap::new(),
            claims: SaveClaimFactory::new(),
        }
    }

    /// This store's shared claim factory
    #[must_use]
    pub fn claim_factory(&self) -> SaveClaimFactory {
        self.claims.clone()
    }

    /// Whether `new_active` differs from the connection this store last saw
    /// as active
    #[must_use]
    pub fn active_connection_changed(&self, new_active: Option<&ActiveConnection>) -> bool {
        new_active != self.last_active_connection.as_ref()
    }

    /// Record `new_key`/`new_active` as the store's current connection and
    /// resolve which snapshot should replace whatever tabs are currently
    /// open.
    ///
    /// Always sets the suppression flag (see [`Self::take_suppressed`]) so
    /// the reload this resolved snapshot feeds into does not trigger its own
    /// save.
    pub fn begin_switch(
        &mut self,
        new_key: Option<ConnectionKey>,
        new_active: Option<ActiveConnection>,
    ) -> Option<Arc<TabSessionSnapshot>> {
        self.last_active_connection = new_active;

        let snapshot = match new_key {
            Some(key) if self.session_cache.contains_key(&key) => {
                self.session_cache.get(&key).cloned()
            }
            Some(key) => match &self.root {
                Some(root) => match SessionDir::new(root, key).load_snapshot() {
                    Ok(snapshot) => snapshot.map(Arc::new),
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "failed to load tab session; falling back to the default tab"
                        );
                        None
                    }
                },
                None => None,
            },
            None => None,
        };

        self.active_key = new_key;
        self.suppress_next_save = true;
        snapshot
    }

    /// Consume the suppression flag set by [`Self::begin_switch`],
    /// resetting it to `false`. Returns `true` exactly once per switch.
    pub fn take_suppressed(&mut self) -> bool {
        std::mem::take(&mut self.suppress_next_save)
    }

    /// Whether [`Self::dispatch_save`] could actually persist anything right
    /// now, i.e. both a tracked active key and a resolved sessions root
    /// exist.
    #[must_use]
    pub fn can_persist(&self) -> bool {
        self.active_key.is_some() && self.root.is_some()
    }

    /// The sessions root and active connection key, for a caller (the Save
    /// modal, a rename request). `None` when either is unresolved.
    #[must_use]
    pub fn active_session_location(&self) -> Option<(PathBuf, ConnectionKey)> {
        Some((self.root.clone()?, self.active_key?))
    }

    /// The active connection's session directory itself
    /// (`root/<key-dir>`)
    #[must_use]
    pub fn active_session_dir(&self) -> Option<PathBuf> {
        let (root, key) = self.active_session_location()?;
        Some(root.join(key.storage_dir_name()))
    }

    /// Record `snapshot` as the active key's latest known state and hand
    /// back what the caller needs to actually write it to disk: the sessions
    /// root, the key, the snapshot itself, and the claim it must write
    /// under. `None` when there is no active key or no resolved root.
    ///
    /// The cache is updated synchronously, before the caller has written
    /// anything to disk, so a switch back to this key sees this session's
    /// own latest tabs even while the actual write is still in flight.
    pub fn dispatch_save(
        &mut self,
        snapshot: TabSessionSnapshot,
    ) -> Option<(PathBuf, ConnectionKey, Arc<TabSessionSnapshot>, SaveClaim)> {
        let key = self.active_key?;
        let root = self.root.clone()?;
        let dir = root.join(key.storage_dir_name());
        let claim = self.claims.mint(&dir);
        tracing::info!(
            key = ?key,
            tab_count = snapshot.tabs.len(),
            claim = claim.value(),
            "dispatching tab session save"
        );
        let snapshot = Arc::new(snapshot);
        self.session_cache.insert(key, Arc::clone(&snapshot));
        Some((root, key, snapshot, claim))
    }
}
