//! Remembered parameter values for the "Run with parameters" modal: one
//! file per session directory, alongside `tabs.toml`, so a script's past
//! values survive a reconnect or app restart the same way its tabs do.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::SessionStoreError;
use super::session_dir::atomic_write;

/// File name of a session directory's remembered parameter values, under
/// the same directory `tabs.toml` lives in.
pub const PARAM_HISTORY_FILE_NAME: &str = "param_history.toml";

/// One script's remembered parameter values, keyed by parameter name, each
/// list ordered most-recent-first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct ScriptParamHistory {
    params: HashMap<String, Vec<String>>,
}

/// A session directory's remembered parameter values, keyed by a script's
/// [`super::ScriptBacking::param_history_key`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParamHistoryFile {
    scripts: HashMap<String, ScriptParamHistory>,
}

impl ParamHistoryFile {
    /// Load `path`, or an empty file if nothing has been persisted there
    /// yet.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if `path` exists but cannot be read or
    /// parsed.
    pub fn load(path: &Path) -> Result<Self, SessionStoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(SessionStoreError::Read)?;
        Ok(toml::from_str(&text)?)
    }

    /// Write this file to `path` via the same atomic temp-file-plus-rename
    /// every other session store write uses.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if serialization or the write fails.
    pub fn save(&self, path: &Path) -> Result<(), SessionStoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SessionStoreError::Write)?;
        }
        let text = toml::to_string_pretty(self)?;
        atomic_write(path, &text)
    }

    /// Record `values` as `script_key`'s most recent run: each parameter's
    /// value moves to the front of its own history (removing a prior equal
    /// entry rather than duplicating it) and the list is capped at
    /// `max_history` entries.
    #[tracing::instrument(name = "param_history_record_run", skip(self, values))]
    pub fn record_run(
        &mut self,
        script_key: &str,
        values: &HashMap<String, String>,
        max_history: usize,
    ) {
        let entry = self.scripts.entry(script_key.to_owned()).or_default();
        let cap = max_history.max(1);
        for (name, value) in values {
            let history = entry.params.entry(name.clone()).or_default();
            history.retain(|existing| existing != value);
            history.insert(0, value.clone());
            history.truncate(cap);
        }
    }

    /// `script_key`'s remembered values for `param_name`, most recent
    /// first. Empty when nothing has been recorded for that pair yet.
    #[must_use]
    pub fn history_for(&self, script_key: &str, param_name: &str) -> &[String] {
        self.scripts
            .get(script_key)
            .and_then(|script| script.params.get(param_name))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::ParamHistoryFile;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-param-history-test-{label}-{}-{n}",
                std::process::id()
            ));
            Self(path)
        }
        fn file(&self) -> std::path::PathBuf {
            self.0.join("param_history.toml")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn loading_a_missing_file_returns_an_empty_history() {
        let temp = TempDir::new("missing");
        let file = ParamHistoryFile::load(&temp.file()).expect("must not error");
        assert!(file.history_for("session:query-1.sql", "status").is_empty());
    }

    #[test]
    fn loading_a_file_with_invalid_toml_returns_a_parse_error_not_a_panic() {
        let temp = TempDir::new("corrupt-load");
        std::fs::create_dir_all(&temp.0).expect("must create dir");
        std::fs::write(temp.file(), b"not valid toml { at all").expect("must write garbage");

        let result = ParamHistoryFile::load(&temp.file());

        assert!(matches!(
            result,
            Err(crate::session_store::SessionStoreError::Parse(_))
        ));
    }

    #[test]
    fn the_session_store_falls_back_to_an_empty_history_over_a_corrupt_file() {
        let temp = TempDir::new("store-corrupt");
        std::fs::create_dir_all(&temp.0).expect("must create dir");
        std::fs::write(temp.file(), b"not valid toml { at all").expect("must write garbage");

        let mut store = crate::session_store::SessionStore::new(None);
        let history = store.param_history(&temp.file());

        assert!(
            history
                .history_for("session:query-1.sql", "status")
                .is_empty()
        );
    }

    #[test]
    fn record_run_then_history_for_returns_the_recorded_value() {
        let mut file = ParamHistoryFile::default();
        file.record_run("session:query-1.sql", &values(&[("status", "shipped")]), 10);
        assert_eq!(
            file.history_for("session:query-1.sql", "status"),
            ["shipped"]
        );
    }

    #[test]
    fn a_repeated_run_moves_the_new_value_to_the_front_most_recent_first() {
        let mut file = ParamHistoryFile::default();
        file.record_run("session:query-1.sql", &values(&[("status", "pending")]), 10);
        file.record_run("session:query-1.sql", &values(&[("status", "shipped")]), 10);
        assert_eq!(
            file.history_for("session:query-1.sql", "status"),
            ["shipped", "pending"]
        );
    }

    #[test]
    fn re_running_with_an_already_remembered_value_deduplicates_rather_than_repeating() {
        let mut file = ParamHistoryFile::default();
        file.record_run("session:query-1.sql", &values(&[("status", "shipped")]), 10);
        file.record_run("session:query-1.sql", &values(&[("status", "pending")]), 10);
        file.record_run("session:query-1.sql", &values(&[("status", "shipped")]), 10);
        assert_eq!(
            file.history_for("session:query-1.sql", "status"),
            ["shipped", "pending"]
        );
    }

    #[test]
    fn history_is_capped_at_max_history() {
        let mut file = ParamHistoryFile::default();
        for value in ["a", "b", "c", "d"] {
            file.record_run("session:query-1.sql", &values(&[("status", value)]), 2);
        }
        assert_eq!(
            file.history_for("session:query-1.sql", "status"),
            ["d", "c"]
        );
    }

    #[test]
    fn history_for_different_scripts_never_mixes() {
        let mut file = ParamHistoryFile::default();
        file.record_run("session:a.sql", &values(&[("status", "shipped")]), 10);
        file.record_run("session:b.sql", &values(&[("status", "cancelled")]), 10);
        assert_eq!(file.history_for("session:a.sql", "status"), ["shipped"]);
        assert_eq!(file.history_for("session:b.sql", "status"), ["cancelled"]);
    }

    #[test]
    fn history_for_different_parameters_on_the_same_script_never_mixes() {
        let mut file = ParamHistoryFile::default();
        file.record_run(
            "session:a.sql",
            &values(&[("status", "shipped"), ("row_limit", "50")]),
            10,
        );
        assert_eq!(file.history_for("session:a.sql", "status"), ["shipped"]);
        assert_eq!(file.history_for("session:a.sql", "row_limit"), ["50"]);
    }

    #[test]
    fn save_then_load_round_trips_recorded_values() {
        let temp = TempDir::new("round-trip");
        let mut file = ParamHistoryFile::default();
        file.record_run("session:orders.sql", &values(&[("status", "shipped")]), 10);
        file.save(&temp.file()).expect("save must succeed");

        let reloaded = ParamHistoryFile::load(&temp.file()).expect("load must succeed");
        assert_eq!(
            reloaded.history_for("session:orders.sql", "status"),
            ["shipped"]
        );
    }
}
