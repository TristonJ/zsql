//! Pure name-validation and path-preview logic for the Save Script and
//! Rename modals, independent of gpui so it is unit-testable without a
//! running app.

use std::path::Path;

use crate::session_store::{SCRIPTS_DIR_NAME, is_unsafe_script_name};

/// The extension every script the Save modal produces carries.
pub const SQL_SUFFIX: &str = crate::session_store::SCRIPT_FILE_EXTENSION;

/// Where a save (or rename) writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    /// The active connection's session directory.
    Connection,
    /// The shared library.
    Library,
    /// An arbitrary path chosen via the platform save-file dialog. Exports a
    /// copy; never retargets the tab, and has no fixed directory to preview
    /// a path against or check for a name collision -- the dialog itself
    /// resolves both.
    External,
}

/// Why a typed name cannot be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    /// The name is empty once whitespace and a trailing `.sql` are
    /// stripped.
    Empty,
    /// The name contains a path separator, or is exactly `..`.
    PathSeparator,
    /// The name contains a colon, which the on-disk `tabs.toml` reserves
    /// for the `library:` ref encoding -- a session file named
    /// `library:foo.sql` would reparse as a library-backed tab on restart.
    Colon,
    /// The chosen destination already has a script with this name.
    Duplicate,
}

impl NameError {
    /// The inline error message the modal shows under the name field.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter a name.",
            Self::PathSeparator => "Name cannot contain a path separator.",
            Self::Colon => "Name cannot contain a colon.",
            Self::Duplicate => "A script with this name already exists here.",
        }
    }
}

/// Trim whitespace and strip every trailing [`SQL_SUFFIX`] the user may have
/// typed (so `report.sql.sql` normalizes to `report`, not `report.sql`),
/// without validating the result. Used by [`preview_path`], which must show
/// something for every keystroke, even an invalid one.
fn normalize_lossy(raw: &str) -> String {
    let mut current = raw.trim();
    loop {
        let stripped = current.strip_suffix(SQL_SUFFIX).unwrap_or(current).trim();
        if stripped == current {
            return current.to_owned();
        }
        current = stripped;
    }
}

/// Validate and normalize a user-typed name: trims whitespace, strips a
/// single trailing `.sql` the user may have typed (rather than rejecting
/// it), and rejects an empty result or one containing a path separator.
/// Never includes the `.sql` extension in its `Ok` result -- callers append
/// [`SQL_SUFFIX`] themselves. Does not check for a duplicate; see
/// [`name_conflicts`].
///
/// # Errors
/// Returns [`NameError::Empty`] if `raw` is empty once trimmed,
/// [`NameError::PathSeparator`] if it contains `/`, `\`, or is exactly
/// `..`, or [`NameError::Colon`] if it contains `:`.
pub fn validate_name(raw: &str) -> Result<String, NameError> {
    let normalized = normalize_lossy(raw);
    if normalized.is_empty() {
        return Err(NameError::Empty);
    }
    // Shared with `session_store::library`'s and `session_store::persistence`'s
    // own on-disk sanitization, so the interactive validator here and the
    // autosave path that derives file names from untrusted tab titles can
    // never disagree about what is safe.
    if is_unsafe_script_name(&normalized) {
        return Err(NameError::PathSeparator);
    }
    if normalized.contains(':') {
        return Err(NameError::Colon);
    }
    Ok(normalized)
}

/// The order [`cycle_destination`] moves through: This connection, Library,
/// then Somewhere else.
const DESTINATION_ORDER: [Destination; 3] = [
    Destination::Connection,
    Destination::Library,
    Destination::External,
];

/// The destination Up/Down navigation moves to from `current`, wrapping at
/// either end -- the save modal's replacement for the untypeable bare
/// `1`/`2`/`3` keybindings, which stole those keystrokes from the name
/// field.
#[must_use]
pub fn cycle_destination(current: Destination, forward: bool) -> Destination {
    let index = DESTINATION_ORDER
        .iter()
        .position(|d| *d == current)
        .unwrap_or(0);
    let len = DESTINATION_ORDER.len();
    let next = if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    };
    DESTINATION_ORDER[next]
}

/// Whether `name`, normalized, already names an existing `.sql` file in the
/// chosen destination -- other than `current_path` itself, if given (the
/// tab's own current file: renaming or re-saving a script to the exact name
/// and destination it already has is never a conflict with itself). `false`
/// (not a conflict) whenever `name` does not validate at all --
/// [`validate_name`]'s own error takes precedence in that case.
///
/// Runs a synchronous [`Path::is_file`] stat directly on the caller's
/// thread rather than a background executor: this checks a single file in
/// the already-resolved session or library directory (the same directories
/// `session_store::mod::SessionStore::begin_switch` reads synchronously for
/// the same reason), and re-runs on every keystroke, so the inline error it
/// feeds must be current by the next frame -- a backgrounded round trip
/// would only reintroduce the exact stale-result race `confirm`'s own
/// fresh re-validation already guards against.
#[must_use]
pub fn name_conflicts(
    name: &str,
    destination: Destination,
    session_dir: &Path,
    library_dir: &Path,
    current_path: Option<&Path>,
) -> bool {
    let Ok(normalized) = validate_name(name) else {
        return false;
    };
    let path = match destination {
        Destination::Connection => session_dir
            .join(SCRIPTS_DIR_NAME)
            .join(format!("{normalized}{SQL_SUFFIX}")),
        Destination::Library => library_dir.join(format!("{normalized}{SQL_SUFFIX}")),
        // Nothing to conflict against: the save-file dialog resolves the
        // destination directory itself, and can warn about an overwrite on
        // its own platform-native terms.
        Destination::External => return false,
    };
    if Some(path.as_path()) == current_path {
        return false;
    }
    path.is_file()
}

/// The single validation the modal's Save button gates on: `Ok(normalized)`
/// with the exact name to save under, or the [`NameError`] to show inline.
/// `current_path` is the tab's own current file (see [`name_conflicts`]),
/// `None` for a tab with no file yet (an unnamed session script's first
/// Save).
///
/// # Errors
/// See [`NameError`].
pub fn validate_for_save(
    name: &str,
    destination: Destination,
    session_dir: &Path,
    library_dir: &Path,
    current_path: Option<&Path>,
) -> Result<String, NameError> {
    let normalized = validate_name(name)?;
    if name_conflicts(name, destination, session_dir, library_dir, current_path) {
        return Err(NameError::Duplicate);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::{
        Destination, NameError, cycle_destination, name_conflicts, validate_for_save, validate_name,
    };

    #[test]
    fn a_plain_name_validates_unchanged() {
        assert_eq!(
            validate_name("top-customers"),
            Ok("top-customers".to_owned())
        );
    }

    #[test]
    fn a_typed_trailing_sql_suffix_is_stripped_not_rejected() {
        assert_eq!(
            validate_name("top-customers.sql"),
            Ok("top-customers".to_owned())
        );
    }

    #[test]
    fn every_trailing_sql_suffix_is_stripped_not_just_the_first() {
        assert_eq!(validate_name("report.sql.sql"), Ok("report".to_owned()));
        assert_eq!(validate_name("report.sql.sql.sql"), Ok("report".to_owned()));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(validate_name("  orders  "), Ok("orders".to_owned()));
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert_eq!(validate_name(""), Err(NameError::Empty));
        assert_eq!(validate_name("   "), Err(NameError::Empty));
        assert_eq!(validate_name(".sql"), Err(NameError::Empty));
    }

    #[test]
    fn a_name_containing_a_forward_slash_is_rejected() {
        assert_eq!(validate_name("a/b"), Err(NameError::PathSeparator));
    }

    #[test]
    fn a_name_starting_with_the_library_ref_prefix_is_rejected() {
        assert_eq!(validate_name("library:foo"), Err(NameError::Colon));
    }

    #[test]
    fn a_name_containing_a_colon_anywhere_is_rejected() {
        assert_eq!(validate_name("report:2024"), Err(NameError::Colon));
    }

    #[test]
    fn a_name_containing_a_backslash_is_rejected() {
        assert_eq!(validate_name("a\\b"), Err(NameError::PathSeparator));
    }

    #[test]
    fn a_name_that_is_exactly_dot_dot_is_rejected() {
        assert_eq!(validate_name(".."), Err(NameError::PathSeparator));
    }

    #[test]
    fn a_name_that_looks_like_an_unnamed_scripts_own_title_is_a_legal_name() {
        // `query-<N>` carries no special meaning to the name validator: an
        // unnamed script's own title never reaches this validator at all,
        // and unnamed-ness is a matter of where a script's file lives, not
        // what it is called -- so a user is free to explicitly name (or
        // rename) a script `query-7`.
        assert_eq!(validate_name("query-7"), Ok("query-7".to_owned()));
        assert_eq!(validate_name("query-1"), Ok("query-1".to_owned()));
        assert_eq!(validate_name("query-"), Ok("query-".to_owned()));
        assert_eq!(validate_name("query-7a"), Ok("query-7a".to_owned()));
        assert_eq!(validate_name("my-query-7"), Ok("my-query-7".to_owned()));
    }

    /// A temp directory this test owns exclusively, removed on drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-save-modal-logic-test-{label}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("must create temp dir");
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_duplicate_name_in_the_selected_destination_is_a_conflict() {
        let session_dir = TempDir::new("dup-session");
        let library_dir = TempDir::new("dup-library");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("orders.sql"),
            "select 1;",
        )
        .expect("must write");

        assert!(name_conflicts(
            "orders",
            Destination::Connection,
            &session_dir.0,
            &library_dir.0,
            None
        ));
    }

    #[test]
    fn the_same_name_in_the_other_destination_is_not_a_conflict() {
        let session_dir = TempDir::new("other-dest-session");
        let library_dir = TempDir::new("other-dest-library");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("orders.sql"),
            "select 1;",
        )
        .expect("must write");

        assert!(!name_conflicts(
            "orders",
            Destination::Library,
            &session_dir.0,
            &library_dir.0,
            None
        ));
    }

    #[test]
    fn a_duplicate_name_in_the_library_is_a_conflict_only_for_the_library_destination() {
        let session_dir = TempDir::new("lib-dup-session");
        let library_dir = TempDir::new("lib-dup-library");
        std::fs::write(library_dir.0.join("revenue.sql"), "select 1;").expect("must write");

        assert!(name_conflicts(
            "revenue",
            Destination::Library,
            &session_dir.0,
            &library_dir.0,
            None
        ));
        assert!(!name_conflicts(
            "revenue",
            Destination::Connection,
            &session_dir.0,
            &library_dir.0,
            None
        ));
    }

    #[test]
    fn a_names_own_current_path_is_never_a_conflict_with_itself() {
        let session_dir = TempDir::new("self-conflict-session");
        let library_dir = TempDir::new("self-conflict-library");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("top-customers.sql"),
            "select 1;",
        )
        .expect("must write");
        let current_path = session_dir.0.join("scripts").join("top-customers.sql");

        assert!(
            !name_conflicts(
                "top-customers",
                Destination::Connection,
                &session_dir.0,
                &library_dir.0,
                Some(&current_path)
            ),
            "a Rename/Save-as modal opened pre-filled with the tab's own current name and \
             destination must not show a duplicate error against itself"
        );
        assert!(
            name_conflicts(
                "top-customers",
                Destination::Connection,
                &session_dir.0,
                &library_dir.0,
                None
            ),
            "the same path is still a real conflict for any other caller with no current path"
        );
    }

    #[test]
    fn validate_for_save_rejects_an_empty_name_before_checking_conflicts() {
        let session_dir = TempDir::new("save-empty-session");
        let library_dir = TempDir::new("save-empty-library");
        assert_eq!(
            validate_for_save(
                "",
                Destination::Connection,
                &session_dir.0,
                &library_dir.0,
                None
            ),
            Err(NameError::Empty)
        );
    }

    #[test]
    fn validate_for_save_rejects_a_name_with_a_path_separator() {
        let session_dir = TempDir::new("save-sep-session");
        let library_dir = TempDir::new("save-sep-library");
        assert_eq!(
            validate_for_save(
                "a/b",
                Destination::Connection,
                &session_dir.0,
                &library_dir.0,
                None
            ),
            Err(NameError::PathSeparator)
        );
    }

    #[test]
    fn validate_for_save_rejects_a_duplicate_name_in_the_chosen_destination() {
        let session_dir = TempDir::new("save-dup-session");
        let library_dir = TempDir::new("save-dup-library");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("orders.sql"),
            "select 1;",
        )
        .expect("must write");

        assert_eq!(
            validate_for_save(
                "orders",
                Destination::Connection,
                &session_dir.0,
                &library_dir.0,
                None
            ),
            Err(NameError::Duplicate)
        );
    }

    #[test]
    fn the_external_destination_never_conflicts_regardless_of_what_exists_on_disk() {
        let session_dir = TempDir::new("external-no-conflict-session");
        let library_dir = TempDir::new("external-no-conflict-library");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("orders.sql"),
            "select 1;",
        )
        .expect("must write");

        assert!(!name_conflicts(
            "orders",
            Destination::External,
            &session_dir.0,
            &library_dir.0,
            None
        ));
    }

    #[test]
    fn validate_for_save_accepts_the_external_destination_with_no_directory_check() {
        let session_dir = TempDir::new("external-save-session");
        let library_dir = TempDir::new("external-save-library");
        assert_eq!(
            validate_for_save(
                "orders",
                Destination::External,
                &session_dir.0,
                &library_dir.0,
                None
            ),
            Ok("orders".to_owned())
        );
    }

    #[test]
    fn cycle_destination_moves_forward_through_connection_library_external_and_wraps() {
        assert_eq!(
            cycle_destination(Destination::Connection, true),
            Destination::Library
        );
        assert_eq!(
            cycle_destination(Destination::Library, true),
            Destination::External
        );
        assert_eq!(
            cycle_destination(Destination::External, true),
            Destination::Connection
        );
    }

    #[test]
    fn cycle_destination_moves_backward_and_wraps_the_other_way() {
        assert_eq!(
            cycle_destination(Destination::Connection, false),
            Destination::External
        );
        assert_eq!(
            cycle_destination(Destination::External, false),
            Destination::Library
        );
    }

    #[test]
    fn validate_for_save_accepts_a_fresh_valid_name() {
        let session_dir = TempDir::new("save-ok-session");
        let library_dir = TempDir::new("save-ok-library");
        assert_eq!(
            validate_for_save(
                "top-customers",
                Destination::Connection,
                &session_dir.0,
                &library_dir.0,
                None
            ),
            Ok("top-customers".to_owned())
        );
    }
}
