//! Pure row-building, filtering, and navigation logic for the Open Script
//! picker, independent of gpui so it is unit-testable without a running app.
//! Mirrors `save_modal/logic.rs`'s split between pure logic and the gpui
//! view.

use super::super::tabs::TabId;

/// One named session script as the picker's "This connection" section sees
/// it -- built from
/// [`crate::session_store::list_session_scripts`], a disk scan of the
/// active connection's session directory, *not* from open tabs: a named
/// script must still be listed (and reopenable) after its tab closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScript {
    /// The exact on-disk sibling file name (already carries `.sql`).
    pub file_name: String,
    pub relative_time: String,
}

/// One library `.sql` file as the picker's "Library" section sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryScript {
    /// Bare name, no `.sql` extension.
    pub name: String,
    pub relative_time: String,
}

/// Which section a row belongs to, for grouped rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerSection {
    Connection,
    Library,
}

/// A row's trailing meta text: "open" for a session tab currently open on
/// this connection or an already-open library tab, or a relative-time
/// string for a script not currently open anywhere on this connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerRowMeta {
    Open,
    RelativeTime(String),
}

/// What selecting/opening a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerTarget {
    /// Focus this already-open tab; never opens a new one.
    FocusTab(TabId),
    /// Open (or, if this exact library name is already open on this
    /// connection, focus) this library file. Resolved to `FocusTab` instead
    /// of this variant at build time whenever a dedupe match exists, so a
    /// caller acting on this target can never create a duplicate tab from a
    /// row this module produced.
    OpenLibrary(String),
    /// Open (or, if already open, focus) this named session script by its
    /// on-disk file name. Resolved to `FocusTab` instead of this variant at
    /// build time whenever a dedupe match exists, the same way
    /// `OpenLibrary` does.
    OpenSessionScript(String),
}

/// One row the picker renders, in the section's own display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub section: PickerSection,
    pub label: String,
    pub meta: PickerRowMeta,
    pub target: PickerTarget,
}

/// Case-insensitive substring match of `needle` (already lowercased) against
/// `label`. An empty `needle` matches everything.
fn matches(label: &str, needle: &str) -> bool {
    needle.is_empty() || label.to_lowercase().contains(needle)
}

/// Build every visible picker row for `filter`, in section order (This
/// connection, then Library), each section preserving the order its input
/// slice was given in.
///
/// `open_session_tabs`/`open_library_tabs` each pair a script's identity
/// (its on-disk file name, or its library name) with the `TabId` of its
/// already-open tab on this connection, if any -- consulted so a row for a
/// script that is already open resolves to [`PickerTarget::FocusTab`]
/// rather than [`PickerTarget::OpenSessionScript`]/[`PickerTarget::OpenLibrary`],
/// never producing a target that would open a duplicate tab.
#[must_use]
pub fn build_rows_with_open_sessions(
    filter: &str,
    session_scripts: &[SessionScript],
    open_session_tabs: &[(String, TabId)],
    library_scripts: &[LibraryScript],
    open_library_tabs: &[(String, TabId)],
) -> Vec<PickerRow> {
    let needle = filter.trim().to_lowercase();
    let mut rows = Vec::with_capacity(session_scripts.len() + library_scripts.len());

    for script in session_scripts {
        if !matches(&script.file_name, &needle) {
            continue;
        }
        let already_open = open_session_tabs
            .iter()
            .find(|(file_name, _)| file_name == &script.file_name)
            .map(|(_, tab_id)| *tab_id);
        let (meta, target) = match already_open {
            Some(tab_id) => (PickerRowMeta::Open, PickerTarget::FocusTab(tab_id)),
            None => (
                PickerRowMeta::RelativeTime(script.relative_time.clone()),
                PickerTarget::OpenSessionScript(script.file_name.clone()),
            ),
        };
        rows.push(PickerRow {
            section: PickerSection::Connection,
            label: script.file_name.clone(),
            meta,
            target,
        });
    }

    for script in library_scripts {
        let label = format!(
            "{}{}",
            script.name,
            crate::session_store::SCRIPT_FILE_EXTENSION
        );
        if !matches(&label, &needle) {
            continue;
        }
        let already_open = open_library_tabs
            .iter()
            .find(|(name, _)| name == &script.name)
            .map(|(_, tab_id)| *tab_id);
        let (meta, target) = match already_open {
            Some(tab_id) => (PickerRowMeta::Open, PickerTarget::FocusTab(tab_id)),
            None => (
                PickerRowMeta::RelativeTime(script.relative_time.clone()),
                PickerTarget::OpenLibrary(script.name.clone()),
            ),
        };
        rows.push(PickerRow {
            section: PickerSection::Library,
            label,
            meta,
            target,
        });
    }

    rows
}

/// The next selected index after an arrow key, advancing (`forward`) or
/// retreating through `rows` in the flat visual order [`build_rows`]
/// produced, wrapping at either end. `None` (no selection) advances to the
/// first row moving forward or the last row moving backward; an empty list
/// has no index to select.
#[must_use]
pub fn navigate(len: usize, current: Option<usize>, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match current {
        None => Some(if forward { 0 } else { len - 1 }),
        Some(index) => Some(if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }),
    }
}

/// The flat child index of `selected` (an index into the picker's own
/// `rows`, connection rows first and library rows after) within the row
/// list's rendered children: the "This connection" header, its rows, the
/// "Library" header, then its rows, in that order. Used to scroll the
/// selected row into view by child index once its section and offset within
/// that section are known.
#[must_use]
pub fn flat_row_index(selected: usize, connection_count: usize) -> usize {
    if selected < connection_count {
        1 + selected
    } else {
        1 + connection_count + 1 + (selected - connection_count)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LibraryScript, PickerRowMeta, PickerSection, PickerTarget, SessionScript,
        build_rows_with_open_sessions, flat_row_index, navigate,
    };

    fn session(file_name: &str, relative_time: &str) -> SessionScript {
        SessionScript {
            file_name: file_name.to_owned(),
            relative_time: relative_time.to_owned(),
        }
    }

    fn library(name: &str, relative_time: &str) -> LibraryScript {
        LibraryScript {
            name: name.to_owned(),
            relative_time: relative_time.to_owned(),
        }
    }

    #[test]
    fn an_empty_filter_shows_every_row_in_both_sections() {
        let sessions = vec![
            session("top-customers.sql", "2s"),
            session("cohort-debug.sql", "1w"),
        ];
        let library = vec![
            library("revenue-report", "2w"),
            library("slow-queries", "1mo"),
        ];

        let rows = build_rows_with_open_sessions("", &sessions, &[], &library, &[]);

        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].section, PickerSection::Connection);
        assert_eq!(rows[0].label, "top-customers.sql");
        assert_eq!(rows[1].label, "cohort-debug.sql");
        assert_eq!(rows[2].section, PickerSection::Library);
        assert_eq!(rows[2].label, "revenue-report.sql");
        assert_eq!(rows[3].label, "slow-queries.sql");
    }

    #[test]
    fn a_named_session_script_with_no_open_tab_shows_its_relative_time_and_targets_open_session_script()
     {
        let sessions = vec![session("top-customers.sql", "2s")];
        let rows = build_rows_with_open_sessions("", &sessions, &[], &[], &[]);
        assert_eq!(rows[0].meta, PickerRowMeta::RelativeTime("2s".to_owned()));
        assert_eq!(
            rows[0].target,
            PickerTarget::OpenSessionScript("top-customers.sql".to_owned())
        );
    }

    #[test]
    fn a_named_session_script_already_open_focuses_the_existing_tab_instead_of_a_duplicate() {
        let sessions = vec![session("top-customers.sql", "2s")];
        let open_session_tabs = vec![("top-customers.sql".to_owned(), 3u64)];

        let rows = build_rows_with_open_sessions("", &sessions, &open_session_tabs, &[], &[]);

        assert_eq!(rows[0].meta, PickerRowMeta::Open);
        assert_eq!(rows[0].target, PickerTarget::FocusTab(3));
    }

    #[test]
    fn a_library_row_not_open_anywhere_shows_its_relative_time_and_targets_open_library() {
        let library = vec![library("revenue-report", "2w")];
        let rows = build_rows_with_open_sessions("", &[], &[], &library, &[]);
        assert_eq!(rows[0].meta, PickerRowMeta::RelativeTime("2w".to_owned()));
        assert_eq!(
            rows[0].target,
            PickerTarget::OpenLibrary("revenue-report".to_owned())
        );
    }

    #[test]
    fn a_substring_filter_matches_case_insensitively_and_excludes_non_matches_from_both_sections() {
        let sessions = vec![
            session("top-customers.sql", "2s"),
            session("cohort-debug.sql", "1w"),
        ];
        let library = vec![
            library("revenue-report", "2w"),
            library("slow-queries", "1mo"),
        ];

        let rows = build_rows_with_open_sessions("REVENUE", &sessions, &[], &library, &[]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "revenue-report.sql");
    }

    #[test]
    fn a_filter_matching_nothing_produces_no_rows_in_either_section() {
        let sessions = vec![session("top-customers.sql", "2s")];
        let library = vec![library("revenue-report", "2w")];
        let rows = build_rows_with_open_sessions("zzz-no-match", &sessions, &[], &library, &[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn opening_a_library_row_already_open_as_a_session_tab_focuses_that_tab_instead_of_a_duplicate()
    {
        let library = vec![library("revenue-report", "2w")];
        let open_library_tabs = vec![("revenue-report".to_owned(), 7u64)];

        let rows = build_rows_with_open_sessions("", &[], &[], &library, &open_library_tabs);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].meta, PickerRowMeta::Open);
        assert_eq!(
            rows[0].target,
            PickerTarget::FocusTab(7),
            "a library row already open on this connection must focus the existing tab, \
             never target OpenLibrary (which would create a duplicate)"
        );
    }

    #[test]
    fn dedupe_only_matches_the_exact_library_name_not_an_unrelated_open_tab() {
        let library = vec![library("revenue-report", "2w")];
        let open_library_tabs = vec![("unrelated-script".to_owned(), 7u64)];

        let rows = build_rows_with_open_sessions("", &[], &[], &library, &open_library_tabs);

        assert_eq!(
            rows[0].target,
            PickerTarget::OpenLibrary("revenue-report".to_owned())
        );
    }

    #[test]
    fn session_dedupe_only_matches_the_exact_file_name_not_an_unrelated_open_tab() {
        let sessions = vec![session("top-customers.sql", "2s")];
        let open_session_tabs = vec![("unrelated.sql".to_owned(), 7u64)];

        let rows = build_rows_with_open_sessions("", &sessions, &open_session_tabs, &[], &[]);

        assert_eq!(
            rows[0].target,
            PickerTarget::OpenSessionScript("top-customers.sql".to_owned())
        );
    }

    #[test]
    fn navigate_with_no_selection_advances_to_the_first_row() {
        assert_eq!(navigate(3, None, true), Some(0));
    }

    #[test]
    fn navigate_with_no_selection_moving_backward_lands_on_the_last_row() {
        assert_eq!(navigate(3, None, false), Some(2));
    }

    #[test]
    fn navigate_forward_wraps_from_the_last_row_to_the_first() {
        assert_eq!(navigate(3, Some(2), true), Some(0));
    }

    #[test]
    fn navigate_backward_wraps_from_the_first_row_to_the_last() {
        assert_eq!(navigate(3, Some(0), false), Some(2));
    }

    #[test]
    fn navigate_advances_across_the_section_boundary_in_visual_order() {
        // Rows 0-1 are "This connection", rows 2-3 are "Library" -- moving
        // forward from the last connection row must land on the first
        // library row, since `navigate` operates on the flat visual order
        // `build_rows` already produced.
        assert_eq!(navigate(4, Some(1), true), Some(2));
    }

    #[test]
    fn navigate_over_an_empty_list_has_nothing_to_select() {
        assert_eq!(navigate(0, None, true), None);
        assert_eq!(navigate(0, None, false), None);
    }

    #[test]
    fn flat_row_index_for_the_first_connection_row_lands_right_after_the_connection_header() {
        assert_eq!(flat_row_index(0, 3), 1);
    }

    #[test]
    fn flat_row_index_for_the_last_connection_row_accounts_for_every_row_before_it() {
        assert_eq!(flat_row_index(2, 3), 3);
    }

    #[test]
    fn flat_row_index_for_the_first_library_row_comes_after_both_headers_and_every_connection_row()
    {
        assert_eq!(flat_row_index(3, 3), 5);
    }

    #[test]
    fn flat_row_index_for_a_later_library_row_accounts_for_every_library_row_before_it() {
        assert_eq!(flat_row_index(4, 3), 6);
    }

    #[test]
    fn flat_row_index_with_an_empty_connection_section_places_the_first_library_row_after_both_headers()
     {
        assert_eq!(flat_row_index(0, 0), 2);
    }

    #[test]
    fn flat_row_index_with_an_empty_connection_section_still_accounts_for_library_rows_before_it() {
        assert_eq!(flat_row_index(2, 0), 4);
    }
}
