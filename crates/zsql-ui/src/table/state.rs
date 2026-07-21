//! Frame-persistent mechanical state behind a [`super::Table`]: the scroll
//! handles both of its panes share, and the [`ScrollableState`] composing
//! them into scrollbars.

use gpui::{AppContext as _, Context, Entity, ScrollHandle, UniformListScrollHandle};

use crate::scrollable::ScrollableState;

/// Frame-persistent scroll and selection state a [`super::Table`] needs
/// across renders. Holds no row or column data -- the table itself is
/// rebuilt fresh from the caller's data every render.
pub struct TableState {
    pub(super) scroll: Entity<ScrollableState>,
    pub(super) row_scroll_handle: UniformListScrollHandle,
    pub(super) col_scroll_handle: ScrollHandle,
    /// The data cell currently selected, as `(data_row, data_col)` indices
    /// into the caller's own data -- never the pinned gutter, never
    /// viewport-relative.
    focused_cell: Option<(usize, usize)>,
}

impl TableState {
    /// Fresh mechanical state, with neither pane yet scrolled and no cell
    /// selected.
    #[must_use]
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            scroll: cx.new(ScrollableState::new),
            row_scroll_handle: UniformListScrollHandle::new(),
            col_scroll_handle: ScrollHandle::new(),
            focused_cell: None,
        }
    }

    /// The scrollable state backing this table's scrollbars, e.g. to query
    /// [`ScrollableState::vertical_visible`]/`horizontal_visible` from a
    /// caller's own test or status logic.
    #[must_use]
    pub fn scroll(&self) -> &Entity<ScrollableState> {
        &self.scroll
    }

    /// The currently selected data cell, as `(data_row, data_col)` indices,
    /// or `None` while nothing is selected.
    #[must_use]
    pub fn focused_cell(&self) -> Option<(usize, usize)> {
        self.focused_cell
    }

    /// Select `(row, col)` as this table's focused cell, replacing any prior
    /// selection. Does not itself notify -- the caller (e.g. a cell's
    /// mouse-down handler) is responsible for calling `cx.notify()`.
    pub fn set_focused_cell(&mut self, row: usize, col: usize) {
        self.focused_cell = Some((row, col));
    }

    /// Clear this table's focused cell, e.g. once the caller's data no
    /// longer has a cell at the previously selected position. Does not
    /// itself notify -- see [`TableState::set_focused_cell`].
    pub fn clear_focused_cell(&mut self) {
        self.focused_cell = None;
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::TableState;

    #[gpui::test]
    fn a_fresh_table_state_has_no_focused_cell(cx: &mut TestAppContext) {
        let state = cx.new(TableState::new);
        state.read_with(cx, |state, _app| {
            assert_eq!(state.focused_cell(), None);
        });
    }

    #[gpui::test]
    fn set_focused_cell_is_read_back_exactly(cx: &mut TestAppContext) {
        let state = cx.new(TableState::new);
        state.update(cx, |state, _cx| state.set_focused_cell(3, 5));
        state.read_with(cx, |state, _app| {
            assert_eq!(state.focused_cell(), Some((3, 5)));
        });
    }

    #[gpui::test]
    fn setting_a_second_cell_replaces_the_first(cx: &mut TestAppContext) {
        let state = cx.new(TableState::new);
        state.update(cx, |state, _cx| {
            state.set_focused_cell(1, 1);
            state.set_focused_cell(2, 4);
        });
        state.read_with(cx, |state, _app| {
            assert_eq!(state.focused_cell(), Some((2, 4)));
        });
    }

    #[gpui::test]
    fn clear_focused_cell_after_a_selection_returns_none(cx: &mut TestAppContext) {
        let state = cx.new(TableState::new);
        state.update(cx, |state, _cx| {
            state.set_focused_cell(0, 0);
            state.clear_focused_cell();
        });
        state.read_with(cx, |state, _app| {
            assert_eq!(state.focused_cell(), None);
        });
    }

    #[gpui::test]
    fn clearing_with_no_prior_selection_stays_none(cx: &mut TestAppContext) {
        let state = cx.new(TableState::new);
        state.update(cx, |state, _cx| state.clear_focused_cell());
        state.read_with(cx, |state, _app| {
            assert_eq!(state.focused_cell(), None);
        });
    }
}
