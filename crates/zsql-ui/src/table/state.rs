//! Frame-persistent mechanical state behind a [`super::Table`]: the scroll
//! handles both of its panes share, and the [`ScrollableState`] composing
//! them into scrollbars.

use gpui::{AppContext as _, Context, Entity, ScrollHandle, UniformListScrollHandle};

use crate::scrollable::{DragHandlers, ScrollableState};

/// Frame-persistent scroll state a [`super::Table`] needs across renders.
/// Holds no row or column data -- the table itself is rebuilt fresh from the
/// caller's data every render.
///
/// A scrollbar thumb-drag started inside the table must keep tracking the
/// pointer even after it leaves the table's own painted bounds, because
/// gpui's mouse-move/mouse-up listeners only fire while the pointer stays
/// within the registering element's own hit-tested bounds. `with_scrollbars`
/// only attaches a thumb's mouse-down, so the caller's own view must attach
/// [`TableState::drag_handlers`]'s listeners at its own render root via
/// `on_mouse_move`/`on_mouse_up`/`on_mouse_up_out`.
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

    /// Mouse-move/mouse-up/mouse-up-out listeners the caller must attach at
    /// their own view's render root -- see this type's own docs for why.
    #[must_use = "the returned listeners do nothing unless attached to the caller's view root"]
    pub fn drag_handlers(&self) -> DragHandlers {
        ScrollableState::drag_handlers(&self.scroll)
    }
}
