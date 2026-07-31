//! How a session exists on disk, shy of the actual I/O: the `tabs.toml`
//! wire format ([`TabsFile`]/[`PersistedTab`]), and script format
//! ([`ScriptRef`]). Also contains the rules for deriving, sanitizing,
//! and disambiguating a script's file name from its tab title

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zsql_core::preview_state::PreviewQueryState;

use super::session_dir::SCRATCH_DIR_NAME;
use crate::session_store::backing::{LibraryName, ScriptFileName};
use crate::session_store::{LIBRARY_FILE_PREFIX, SCRIPT_FILE_EXTENSION, ScriptBacking};

/// Current [`TabsFile::version`]. Bumped whenever a shape change requires a
/// migration.
pub(crate) const CURRENT_TABS_FILE_VERSION: u32 = 1;

/// The on-disk shape of a session directory's `tabs.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TabsFile {
    /// Position in `tabs` of the tab that was active, if any.
    pub(crate) active: Option<usize>,
    pub(crate) tabs: Vec<PersistedTab>,
    /// Version marker - defaults to 0 if missing.
    #[serde(default)]
    pub(crate) version: u32,
}

/// One persisted tab, as stored in `tabs.toml`. A `Script` entry carries
/// only its sibling buffer file's bare name
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PersistedTab {
    Script {
        title: String,
        file: String,
        /// The tab's session-scoped draft file, present only while a
        /// library- or external-backed tab's buffer diverges from its real
        /// file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        draft: Option<String>,
    },
    Generated {
        title: String,
        schema: String,
        relation: String,
        preview_state: PreviewQueryState,
    },
}

/// The three shapes a `tabs.toml` `Script` entry's `file` ref can take for a
/// script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScriptRef {
    /// A session script: `scripts/<name>.sql`.
    Session(ScriptFileName),
    /// A `scratch/` script: `scratch/<name>.sql`.
    Scratch(ScriptFileName),
    /// A library file: `library:<name>.sql`.
    Library(LibraryName),
}

impl ScriptRef {
    /// Parse a session/scratch/library ref. Returns `None` for anything
    /// that is not a valid instance of one of the three shapes
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        if let Some(name) = raw.strip_prefix(LIBRARY_FILE_PREFIX) {
            let name = name.strip_suffix(SCRIPT_FILE_EXTENSION).unwrap_or(name);
            return LibraryName::new(name).ok().map(ScriptRef::Library);
        }
        let scratch_prefix = format!("{SCRATCH_DIR_NAME}/");
        if let Some(name) = raw.strip_prefix(&scratch_prefix) {
            return ScriptFileName::new(name).ok().map(ScriptRef::Scratch);
        }
        ScriptFileName::new(raw).ok().map(ScriptRef::Session)
    }

    pub(crate) fn to_ref_string(&self) -> String {
        match self {
            ScriptRef::Session(file) => file.as_str().to_owned(),
            ScriptRef::Scratch(file) => format!("{SCRATCH_DIR_NAME}/{}", file.as_str()),
            ScriptRef::Library(name) => {
                format!(
                    "{LIBRARY_FILE_PREFIX}{}{SCRIPT_FILE_EXTENSION}",
                    name.as_str()
                )
            }
        }
    }
}

/// Prefix an external file ref carries in `tabs.toml` when its path is not
/// valid UTF-8 and had to be encoded losslessly as hex-encoded raw bytes.
const EXTERNAL_BYTES_PREFIX: &str = "bytes:";
/// Prefix a library tab's draft file carries
const LIBRARY_DRAFT_PREFIX: &str = "library-";
/// Prefix an external tab's draft file carries
const EXTERNAL_DRAFT_PREFIX: &str = "external-";

/// Whether a `Script` entry's `file` ref names an external file (an
/// absolute path)
pub(crate) fn is_external_ref(file: &str) -> bool {
    file.starts_with(EXTERNAL_BYTES_PREFIX) || Path::new(file).is_absolute()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

#[cfg(unix)]
pub(crate) fn encode_external_ref(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    match path.to_str() {
        Some(text) => text.to_owned(),
        None => format!(
            "{EXTERNAL_BYTES_PREFIX}{}",
            hex_encode(path.as_os_str().as_bytes())
        ),
    }
}

#[cfg(not(unix))]
pub(crate) fn encode_external_ref(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(unix)]
pub(crate) fn decode_external_ref(raw: &str) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    if let Some(hex) = raw.strip_prefix(EXTERNAL_BYTES_PREFIX)
        && let Some(bytes) = hex_decode(hex)
    {
        return PathBuf::from(std::ffi::OsStr::from_bytes(&bytes));
    }
    PathBuf::from(raw)
}

#[cfg(not(unix))]
pub(crate) fn decode_external_ref(raw: &str) -> PathBuf {
    PathBuf::from(raw)
}

/// A stable draft file name for a tab. None for session tabs.
pub(crate) fn draft_file_name(backing: &ScriptBacking) -> Option<String> {
    match backing {
        ScriptBacking::Library { name, .. } => Some(format!(
            "{LIBRARY_DRAFT_PREFIX}{}{SCRIPT_FILE_EXTENSION}",
            sanitize_script_title(name.as_str())
        )),
        ScriptBacking::External { path, .. } => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            path.as_os_str().hash(&mut hasher);
            Some(format!(
                "{EXTERNAL_DRAFT_PREFIX}{:016x}{SCRIPT_FILE_EXTENSION}",
                hasher.finish()
            ))
        }
        ScriptBacking::SessionScratch { .. } | ScriptBacking::SessionNamed { .. } => None,
    }
}

/// Migrate every legacy-shaped `Script` entry in `tabs_file` into `scratch/`:
/// moves the sibling file on disk and rewrites the entry's `file` ref. Returns
/// whether anything was migrated.
pub(crate) fn migrate_legacy_unnamed_scripts(dir: &Path, tabs_file: &mut TabsFile) -> bool {
    use std::fs;
    let mut migrated = false;
    for tab in &mut tabs_file.tabs {
        let PersistedTab::Script { title, file, .. } = tab else {
            continue;
        };
        if title != file || !is_legacy_unnamed_script_file_name(file) {
            continue;
        }
        let scratch_dir = dir.join(SCRATCH_DIR_NAME);
        if let Err(err) = fs::create_dir_all(&scratch_dir) {
            tracing::warn!(
                file = %file,
                error = %err,
                "failed to create scratch/ while migrating a legacy unnamed script; \
                 leaving it at the top level"
            );
            continue;
        }
        if let Err(err) = fs::rename(dir.join(&file), scratch_dir.join(&file)) {
            tracing::warn!(
                file = %file,
                error = %err,
                "failed to migrate a legacy unnamed script into scratch/; leaving it at \
                 the top level"
            );
            continue;
        }
        tracing::info!(file = %file, "migrated a legacy unnamed session script into scratch/");
        *file = format!("{SCRATCH_DIR_NAME}/{file}");
        migrated = true;
    }
    migrated
}

/// Leading text of the bare filename scripts carry before save gives one an explicit
/// name
pub(super) const UNNAMED_SCRIPT_PREFIX: &str = "query-";

/// Whether `name` is unsafe to join onto a directory as a bare filename
/// component
#[must_use]
pub(crate) fn is_unsafe_script_name(name: &str) -> bool {
    name.contains('/') || name.contains('\\') || name == ".."
}

/// Sanitize `title` into a name safe to join onto a session directory
fn sanitize_script_title(title: &str) -> String {
    if !is_unsafe_script_name(title) {
        return title.to_owned();
    }
    let mut sanitized: String = title
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    if sanitized == ".." {
        "_.".clone_into(&mut sanitized);
    }
    sanitized
}

/// The sibling buffer file name for a script tab titled `title`
pub(crate) fn script_file_name(title: &str) -> String {
    let safe = sanitize_script_title(title);
    if safe
        .to_ascii_lowercase()
        .ends_with(&SCRIPT_FILE_EXTENSION.to_ascii_lowercase())
    {
        safe
    } else {
        format!("{safe}{SCRIPT_FILE_EXTENSION}")
    }
}

/// `candidate` (an already-complete bare file name, extension included),
/// disambiguated against every name already in `used` (compared
/// case-insensitively) by appending a `-2`, `-3`, ... counter before the
/// extension. Returns `candidate` itself unchanged when it is not already
/// taken.
pub(crate) fn disambiguate_file_name(candidate: &str, used: &HashSet<String>) -> String {
    let used_lower: HashSet<String> = used.iter().map(|name| name.to_ascii_lowercase()).collect();
    if !used_lower.contains(&candidate.to_ascii_lowercase()) {
        return candidate.to_owned();
    }
    let stem = candidate
        .strip_suffix(SCRIPT_FILE_EXTENSION)
        .unwrap_or(candidate);
    let mut suffix = 2usize;
    loop {
        let attempt = format!("{stem}-{suffix}{SCRIPT_FILE_EXTENSION}");
        if !used_lower.contains(&attempt.to_ascii_lowercase()) {
            return attempt;
        }
        suffix += 1;
    }
}

/// [`script_file_name`] for `title`, disambiguated against every name
/// already in `used` (compared case-insensitively)
pub(crate) fn unique_script_file_name(title: &str, used: &HashSet<String>) -> String {
    disambiguate_file_name(&script_file_name(title), used)
}

/// Whether `file_name` matches the bare top-level shape a legacy `tabs.toml`
/// (or a legacy `tab_sessions.json` entry's title) gives an unnamed script's
/// sibling file (`query-<digits>.sql`). Meaningful only for one-time
/// migration on load ([`migrate_legacy_unnamed_scripts`],
/// [`crate::session_store::migration`]).
pub(crate) fn is_legacy_unnamed_script_file_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower
        .strip_prefix(UNNAMED_SCRIPT_PREFIX)
        .and_then(|rest| rest.strip_suffix(SCRIPT_FILE_EXTENSION))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        LibraryName, PersistedTab, ScriptBacking, ScriptFileName, ScriptRef, TabsFile,
        decode_external_ref, draft_file_name, encode_external_ref, hex_decode,
        is_legacy_unnamed_script_file_name, migrate_legacy_unnamed_scripts, script_file_name,
        unique_script_file_name,
    };

    #[test]
    fn unique_script_file_name_treats_names_differing_only_by_case_as_colliding() {
        let mut used = std::collections::HashSet::new();
        used.insert("Orders.sql".to_owned());
        let name = unique_script_file_name("orders", &used);
        assert_eq!(
            name, "orders-2.sql",
            "a case-only difference must still be treated as a collision"
        );
    }

    #[test]
    fn script_file_name_does_not_double_up_an_existing_suffix_regardless_of_its_case() {
        assert_eq!(script_file_name("REPORT.SQL"), "REPORT.SQL");
        assert_eq!(script_file_name("Report.Sql"), "Report.Sql");
    }

    #[test]
    fn is_legacy_unnamed_script_file_name_matches_the_pattern_case_insensitively() {
        assert!(is_legacy_unnamed_script_file_name("query-1.sql"));
        assert!(
            is_legacy_unnamed_script_file_name("query-1.SQL"),
            "an uppercased extension must still be recognized as the legacy unnamed pattern"
        );
        assert!(is_legacy_unnamed_script_file_name("QUERY-1.sql"));
        assert!(!is_legacy_unnamed_script_file_name("top-customers.sql"));
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("zsql-wire-test-{label}-{}-{n}", std::process::id()));
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn legacy_entry(title: &str, file: &str) -> PersistedTab {
        PersistedTab::Script {
            title: title.to_owned(),
            file: file.to_owned(),
            draft: None,
        }
    }

    #[test]
    fn a_session_ref_round_trips_through_parse_and_to_ref_string() {
        let file = ScriptFileName::new("orders.sql").unwrap();
        let parsed = ScriptRef::parse(&ScriptRef::Session(file.clone()).to_ref_string());
        assert_eq!(parsed, Some(ScriptRef::Session(file)));
    }

    #[test]
    fn a_scratch_ref_round_trips_through_parse_and_to_ref_string() {
        let file = ScriptFileName::new("query-1.sql").unwrap();
        let parsed = ScriptRef::parse(&ScriptRef::Scratch(file.clone()).to_ref_string());
        assert_eq!(parsed, Some(ScriptRef::Scratch(file)));
    }

    #[test]
    fn a_library_ref_round_trips_through_parse_and_to_ref_string() {
        let name = LibraryName::new("revenue-report").unwrap();
        let parsed = ScriptRef::parse(&ScriptRef::Library(name.clone()).to_ref_string());
        assert_eq!(parsed, Some(ScriptRef::Library(name)));
    }

    #[test]
    fn parse_rejects_an_empty_ref() {
        assert_eq!(ScriptRef::parse(""), None);
    }

    /// Every generated valid name, for each of the three ref shapes, parses
    /// back to exactly the variant and value it was built from.
    #[test]
    fn generated_names_round_trip_through_parse_and_to_ref_string_for_every_ref_shape() {
        let stems = [
            "orders",
            "top-customers",
            "a.b.c",
            "UPPER",
            "notes",
            "q1",
            "report-2026",
            "under_score",
            "MiXeD-Case",
            "x",
            ".hidden",
            "..leading-dots",
            "scratch",
            "scratchy",
            "library",
            "librarian",
            "a",
            "1",
        ];
        let extensions = [".sql", ".SQL", ".Sql"];
        for stem in stems {
            for ext in extensions {
                let name = format!("{stem}{ext}");

                if let Ok(file) = ScriptFileName::new(name.clone()) {
                    let session_ref = ScriptRef::Session(file.clone());
                    assert_eq!(
                        ScriptRef::parse(&session_ref.to_ref_string()),
                        Some(session_ref)
                    );
                    let scratch_ref = ScriptRef::Scratch(file);
                    assert_eq!(
                        ScriptRef::parse(&scratch_ref.to_ref_string()),
                        Some(scratch_ref)
                    );
                }

                if let Ok(library_name) = LibraryName::new(stem) {
                    let library_ref = ScriptRef::Library(library_name);
                    assert_eq!(
                        ScriptRef::parse(&library_ref.to_ref_string()),
                        Some(library_ref)
                    );
                }
            }
        }
    }

    /// A minimal xorshift PRNG, seeded per call so the sequence is
    /// reproducible across runs without pulling in an external property-test
    /// crate this workspace does not otherwise depend on.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// A pseudo-random ASCII name built from the same character classes a
    /// real script or library name draws from (letters, digits, `-`, `_`,
    /// `.`), of a pseudo-random length between 1 and 12.
    fn random_name(state: &mut u64) -> String {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.";
        // Every modulus below is a small compile-time constant, so the
        // result always fits in `usize` regardless of pointer width.
        let len = 1 + usize::try_from(xorshift(state) % 12).unwrap_or(0);
        (0..len)
            .map(|_| {
                let index = usize::try_from(xorshift(state) % CHARS.len() as u64).unwrap_or(0);
                CHARS[index] as char
            })
            .collect()
    }

    /// Property: for any pseudo-randomly generated name, if it constructs a
    /// valid `ScriptFileName`/`LibraryName` at all, the corresponding
    /// `ScriptRef` round-trips through `to_ref_string`/`parse` back to
    /// exactly itself -- regardless of which arbitrary characters made it
    /// valid in the first place.
    #[test]
    fn arbitrary_valid_names_round_trip_through_parse_and_to_ref_string() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..2000 {
            let stem = random_name(&mut state);
            let name = format!("{stem}.sql");

            if let Ok(file) = ScriptFileName::new(name.clone()) {
                let session_ref = ScriptRef::Session(file.clone());
                assert_eq!(
                    ScriptRef::parse(&session_ref.to_ref_string()),
                    Some(session_ref),
                    "session ref round trip failed for stem {stem:?}"
                );
                let scratch_ref = ScriptRef::Scratch(file);
                assert_eq!(
                    ScriptRef::parse(&scratch_ref.to_ref_string()),
                    Some(scratch_ref),
                    "scratch ref round trip failed for stem {stem:?}"
                );
            }

            if let Ok(library_name) = LibraryName::new(stem.clone()) {
                let library_ref = ScriptRef::Library(library_name);
                assert_eq!(
                    ScriptRef::parse(&library_ref.to_ref_string()),
                    Some(library_ref),
                    "library ref round trip failed for stem {stem:?}"
                );
            }
        }
    }

    #[test]
    fn hex_decode_rejects_an_odd_length_string() {
        assert_eq!(hex_decode("abc"), None);
    }

    #[test]
    fn hex_decode_rejects_a_string_containing_a_non_hex_digit() {
        assert_eq!(hex_decode("zz"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_external_path_round_trips_exactly_through_encode_and_decode() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;

        let path = Path::new(OsStr::from_bytes(b"/tmp/\xffx.sql"));
        let encoded = encode_external_ref(path);
        let decoded = decode_external_ref(&encoded);
        assert_eq!(decoded, path);
    }

    #[test]
    fn draft_file_name_for_an_external_tab_is_external_prefix_plus_sixteen_lowercase_hex_digits_and_the_sql_extension()
     {
        let backing = ScriptBacking::External {
            path: PathBuf::from("/home/user/scratch.sql"),
            saved_text: None,
        };
        let name =
            draft_file_name(&backing).expect("external backing always has a draft file name");
        let hash_part = name
            .strip_prefix("external-")
            .and_then(|rest| rest.strip_suffix(".sql"))
            .expect("draft file name for an external tab must be external-<hash>.sql");
        assert_eq!(hash_part.len(), 16);
        assert!(
            hash_part
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash part {hash_part:?} must be all lowercase hex digits"
        );
    }

    #[test]
    fn draft_file_name_for_the_same_external_path_is_stable_across_calls() {
        let backing = ScriptBacking::External {
            path: PathBuf::from("/home/user/scratch.sql"),
            saved_text: None,
        };
        let first =
            draft_file_name(&backing).expect("external backing always has a draft file name");
        let second =
            draft_file_name(&backing).expect("external backing always has a draft file name");
        assert_eq!(first, second);
    }

    #[test]
    fn draft_file_name_for_two_different_external_paths_never_collides() {
        let a = ScriptBacking::External {
            path: PathBuf::from("/home/user/one.sql"),
            saved_text: None,
        };
        let b = ScriptBacking::External {
            path: PathBuf::from("/home/user/two.sql"),
            saved_text: None,
        };
        let name_a = draft_file_name(&a).expect("external backing always has a draft file name");
        let name_b = draft_file_name(&b).expect("external backing always has a draft file name");
        assert_ne!(name_a, name_b);
    }

    #[test]
    fn draft_file_name_for_two_different_library_names_never_collides() {
        let a = ScriptBacking::Library {
            name: LibraryName::new("orders").unwrap(),
            saved_text: None,
        };
        let b = ScriptBacking::Library {
            name: LibraryName::new("revenue").unwrap(),
            saved_text: None,
        };
        let name_a = draft_file_name(&a).expect("library backing always has a draft file name");
        let name_b = draft_file_name(&b).expect("library backing always has a draft file name");
        assert_ne!(name_a, name_b);
    }

    #[test]
    fn a_legacy_entry_whose_title_and_file_agree_and_match_the_unnamed_pattern_migrates_into_scratch()
     {
        let temp = TempDir::new("legacy-migration");
        std::fs::create_dir_all(&temp.0).expect("must create session dir");
        std::fs::write(temp.0.join("query-1.sql"), "select 'legacy';")
            .expect("must write the legacy top-level sibling file");
        let mut tabs_file = TabsFile {
            active: Some(0),
            tabs: vec![legacy_entry("query-1.sql", "query-1.sql")],
            version: 0,
        };

        let migrated = migrate_legacy_unnamed_scripts(&temp.0, &mut tabs_file);

        assert!(migrated);
        assert!(
            !temp.0.join("query-1.sql").exists(),
            "the legacy top-level file must be moved, not copied"
        );
        let content = std::fs::read_to_string(temp.0.join("scratch").join("query-1.sql"))
            .expect("the file must now live under scratch/");
        assert_eq!(content, "select 'legacy';");
        assert!(matches!(
            &tabs_file.tabs[0],
            PersistedTab::Script { file, .. } if file == "scratch/query-1.sql"
        ));
    }

    #[test]
    fn a_legacy_entry_whose_title_and_file_differ_is_left_at_the_top_level() {
        let temp = TempDir::new("legacy-migration-title-file-mismatch");
        std::fs::create_dir_all(&temp.0).expect("must create session dir");
        std::fs::write(temp.0.join("query-1-2.sql"), "select 'disambiguated';")
            .expect("must write the disambiguated top-level sibling file");
        let mut tabs_file = TabsFile {
            active: Some(0),
            tabs: vec![legacy_entry("query-1.sql", "query-1-2.sql")],
            version: 0,
        };

        let migrated = migrate_legacy_unnamed_scripts(&temp.0, &mut tabs_file);

        assert!(
            !migrated,
            "a title/file mismatch must never be treated as the legacy unnamed shape"
        );
        assert!(
            temp.0.join("query-1-2.sql").is_file(),
            "the mismatched entry's file must stay at the top level, not move into scratch/"
        );
        assert!(!temp.0.join("scratch").join("query-1-2.sql").exists());
        assert!(matches!(
            &tabs_file.tabs[0],
            PersistedTab::Script { file, .. } if file == "query-1-2.sql"
        ));
    }

    #[test]
    fn a_legacy_entry_whose_sibling_file_is_missing_is_skipped_without_touching_the_rest() {
        let temp = TempDir::new("legacy-migration-missing-sibling");
        std::fs::create_dir_all(&temp.0).expect("must create session dir");
        std::fs::write(temp.0.join("top-customers.sql"), "select * from customers;")
            .expect("must write the second, readable entry's sibling file");
        let mut tabs_file = TabsFile {
            active: Some(0),
            tabs: vec![
                legacy_entry("query-1.sql", "query-1.sql"),
                legacy_entry("top-customers.sql", "top-customers.sql"),
            ],
            version: 0,
        };

        let migrated = migrate_legacy_unnamed_scripts(&temp.0, &mut tabs_file);

        assert!(
            !migrated,
            "the only migratable-shaped entry's sibling is missing, so nothing actually moved"
        );
        assert!(
            !temp.0.join("scratch").join("query-1.sql").exists(),
            "a failed migration attempt must never resurrect the missing sibling under scratch/"
        );
        assert!(matches!(
            &tabs_file.tabs[0],
            PersistedTab::Script { file, .. } if file == "query-1.sql"
        ));
        assert!(matches!(
            &tabs_file.tabs[1],
            PersistedTab::Script { file, .. } if file == "top-customers.sql"
        ));
    }

    #[test]
    fn a_legacy_entry_stays_at_the_top_level_when_a_file_named_scratch_blocks_the_directory() {
        let temp = TempDir::new("legacy-migration-scratch-blocked");
        std::fs::create_dir_all(&temp.0).expect("must create session dir");
        std::fs::write(temp.0.join("scratch"), "not a directory")
            .expect("must write a regular file named scratch");
        std::fs::write(temp.0.join("query-1.sql"), "select 1;").expect("must write the script");
        let mut tabs_file = TabsFile {
            active: Some(0),
            tabs: vec![legacy_entry("query-1.sql", "query-1.sql")],
            version: 0,
        };

        let migrated = migrate_legacy_unnamed_scripts(&temp.0, &mut tabs_file);

        assert!(!migrated);
        assert!(
            temp.0.join("query-1.sql").exists(),
            "the legacy script is left at the top level when scratch/ cannot be created"
        );
        assert_eq!(
            std::fs::read_to_string(temp.0.join("scratch")).expect("scratch must still be a file"),
            "not a directory",
            "the blocking file is never clobbered by the migration"
        );
    }
}
