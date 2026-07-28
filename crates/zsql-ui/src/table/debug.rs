//! Debug-selector tagging for the table's gutter, header, and body cells:
//! per-entity lookup keys a render test finds via
//! `VisualTestContext::debug_bounds` to confirm a pane's painted content
//! actually moved in step with a scroll/drag, rather than only checking
//! that a shared scroll handle's offset changed.

#[cfg(any(test, feature = "test-support"))]
use gpui::SharedString;
use gpui::{Div, Entity, prelude::*};

use super::state::TableState;

#[cfg(any(test, feature = "test-support"))]
fn gutter_first_cell_id(state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!("zsql-ui-table-gutter-cell0-{}", state.entity_id()))
}

#[cfg(any(test, feature = "test-support"))]
fn header_first_cell_id(state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!("zsql-ui-table-header-cell0-{}", state.entity_id()))
}

#[cfg(any(test, feature = "test-support"))]
fn body_first_cell_id(state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!("zsql-ui-table-body-cell0-{}", state.entity_id()))
}

/// Tags `cell` if `ix` is the row currently at the top of the visible
/// range (`top_of_viewport`) with a lookup key for
/// `VisualTestContext::debug_bounds`, i.e. whichever gutter row a render
/// test can reliably find painted every frame regardless of how far the
/// list has scrolled, so render tests can confirm the gutter actually
/// moves in step with a vertical scroll/drag rather than only checking
/// that the shared scroll handle's offset changed. Every other cell passes
/// through unchanged. A no-op outside test builds.
pub(super) fn tag_first_gutter_cell(
    cell: Div,
    ix: usize,
    top_of_viewport: usize,
    state: &Entity<TableState>,
) -> Div {
    #[cfg(any(test, feature = "test-support"))]
    {
        if ix == top_of_viewport {
            let selector = gutter_first_cell_id(state).to_string();
            return cell.debug_selector(move || selector.clone());
        }
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        let _ = (ix, top_of_viewport, state);
    }
    cell
}

/// Tags the header row's first cell with a lookup key for
/// `VisualTestContext::debug_bounds`, so render tests can confirm the header
/// actually moves in step with a horizontal scroll/drag rather than only
/// checking that the shared scroll handle's offset changed.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn tag_header_cell(cell: Div, index: usize, state: &Entity<TableState>) -> Div {
    if index == 0 {
        let selector = header_first_cell_id(state).to_string();
        cell.debug_selector(move || selector.clone())
    } else {
        cell
    }
}

/// A no-op outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn tag_header_cell(cell: Div, _index: usize, _state: &Entity<TableState>) -> Div {
    cell
}

/// Tags the top-of-viewport body row's first cell (`cell_index == 0` and
/// `row_index == top_of_viewport`) with a lookup key for
/// `VisualTestContext::debug_bounds`, so render tests can confirm the data
/// body actually moves in step with the header/gutter rather than only
/// checking that a shared scroll handle's offset changed. Every other cell
/// passes through unchanged. A no-op outside test builds.
pub(super) fn tag_first_body_cell<E: InteractiveElement>(
    cell: E,
    cell_index: usize,
    row_index: usize,
    top_of_viewport: usize,
    state: &Entity<TableState>,
) -> E {
    #[cfg(any(test, feature = "test-support"))]
    {
        if cell_index == 0 && row_index == top_of_viewport {
            let selector = body_first_cell_id(state).to_string();
            return cell.debug_selector(move || selector.clone());
        }
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        let _ = (cell_index, row_index, top_of_viewport, state);
    }
    cell
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// gutter's first visible row-number cell, for a consumer crate's own render
/// tests. Requires this crate's `test-support` feature (or building this
/// crate's own tests).
///
/// The returned `&'static str` is deliberately leaked:
/// `VisualTestContext::debug_bounds` takes `&'static str`, and the key is
/// per-entity so it cannot be a literal. Test-support builds only, and one
/// small leak per call.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn gutter_first_cell_debug_selector(state: &Entity<TableState>) -> &'static str {
    Box::leak(gutter_first_cell_id(state).to_string().into_boxed_str())
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// header row's first cell, for a consumer crate's own render tests.
/// Requires this crate's `test-support` feature (or building this crate's
/// own tests).
///
/// The returned `&'static str` is deliberately leaked: see
/// [`gutter_first_cell_debug_selector`] for why.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn header_first_cell_debug_selector(state: &Entity<TableState>) -> &'static str {
    Box::leak(header_first_cell_id(state).to_string().into_boxed_str())
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// top-of-viewport body row's first cell, for a consumer crate's own render
/// tests. Requires this crate's `test-support` feature (or building this
/// crate's own tests).
///
/// The returned `&'static str` is deliberately leaked: see
/// [`gutter_first_cell_debug_selector`] for why.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn body_first_cell_debug_selector(state: &Entity<TableState>) -> &'static str {
    Box::leak(body_first_cell_id(state).to_string().into_boxed_str())
}
