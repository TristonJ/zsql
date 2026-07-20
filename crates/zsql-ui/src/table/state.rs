//! Frame-persistent mechanical state behind a [`super::Table`]: the scroll
//! handles both of its panes share, and the [`ScrollableState`] composing
//! them into scrollbars.

use gpui::{AppContext as _, Context, Entity, ScrollHandle, UniformListScrollHandle};

use crate::scrollable::ScrollableState;

/// Frame-persistent scroll state a [`super::Table`] needs across renders.
/// Holds no row or column data -- the table itself is rebuilt fresh from the
/// caller's data every render.
pub struct TableState {
    pub(super) scroll: Entity<ScrollableState>,
    pub(super) row_scroll_handle: UniformListScrollHandle,
    pub(super) col_scroll_handle: ScrollHandle,
}

impl TableState {
    /// Fresh mechanical state, with neither pane yet scrolled.
    #[must_use]
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            scroll: cx.new(ScrollableState::new),
            row_scroll_handle: UniformListScrollHandle::new(),
            col_scroll_handle: ScrollHandle::new(),
        }
    }

    /// The scrollable state backing this table's scrollbars, e.g. to query
    /// [`ScrollableState::vertical_visible`]/`horizontal_visible` from a
    /// caller's own test or status logic.
    #[must_use]
    pub fn scroll(&self) -> &Entity<ScrollableState> {
        &self.scroll
    }
}
