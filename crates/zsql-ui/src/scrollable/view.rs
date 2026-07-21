//! A batteries-included vertically scrolling page: one container that scrolls
//! its whole content vertically, with an overlaid scrollbar, sizing itself
//! from what it holds rather than from a caller-supplied height.

use gpui::{
    App, Context, Div, ElementId, Entity, IntoElement, Render, ScrollHandle, div, prelude::*,
};

use super::axis::Axis;
use super::source::ScrollSource;
use super::state::ScrollableState;
use super::style::ScrollbarStyle;
use super::wrapper::WithScrollbars;

/// Frame-persistent state a [`vertical_scroll`] page needs: the scrollbar's
/// own [`ScrollableState`] and the container scroll handle its content is
/// tracked by. A view owns one of these per scrolling page.
pub struct ScrollView {
    scroll: Entity<ScrollableState>,
    handle: ScrollHandle,
}

impl ScrollView {
    /// A fresh, unscrolled page.
    #[must_use]
    pub fn new(cx: &mut App) -> Self {
        Self {
            scroll: cx.new(ScrollableState::new),
            handle: ScrollHandle::new(),
        }
    }

    /// The scrollbar state, for a consumer crate's own render tests (the
    /// scrollbar-thumb debug selectors take it).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn scroll_state(&self) -> &Entity<ScrollableState> {
        &self.scroll
    }
}

/// Wrap `content` in a page that scrolls vertically as a whole and never
/// horizontally, with an overlaid vertical scrollbar.
///
/// The page sizes its scrollable extent from `content` itself -- the axis is
/// [`Axis::measured`], so the caller supplies no height. Plain vertical
/// wheel scrolling is gpui's own (the container is `overflow_y_scroll` with a
/// tracked handle); the overlay is this crate's styled scrollbar.
///
/// `content` must be a single element that takes its natural height and does
/// not shrink -- its height coming from its children (e.g. a `flex_col` of
/// `TableSizing::Fit` tables, which grow to all their rows, or plain blocks)
/// with `.flex_shrink_0()` applied. Two shapes break scrolling: a `flex_1`/`h_full` element stretches
/// to the viewport and can never exceed it, and a shrinkable flex child is
/// squeezed down to the viewport by the flex column. Either way nothing
/// overflows and the page never scrolls.
///
/// Drag tracking needs no wiring from the caller -- the scrollbar registers
/// its own window-level listeners while a thumb-drag is in progress.
#[must_use]
pub fn vertical_scroll<V: Render>(
    id: impl Into<ElementId>,
    view: &ScrollView,
    style: ScrollbarStyle,
    content: impl IntoElement,
    cx: &mut Context<V>,
) -> Div {
    view.scroll.update(cx, |scroll, _cx| {
        scroll.vertical(Axis::measured(ScrollSource::Container(view.handle.clone())));
        scroll.clear_horizontal();
    });

    div()
        .id(id.into())
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .h_full()
        .overflow_y_scroll()
        .track_scroll(&view.handle)
        .child(content)
        .with_scrollbars(&view.scroll, style, cx)
}

#[cfg(test)]
mod tests {
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, div, prelude::*, px};

    use super::{ScrollView, vertical_scroll};
    use crate::scrollable::{ScrollbarStyle, vertical_thumb_debug_selector};

    const PANE: f32 = 200.0;

    struct Harness {
        view: ScrollView,
        content_height: f32,
    }

    impl Harness {
        fn new(content_height: f32, cx: &mut Context<Self>) -> Self {
            Self {
                view: ScrollView::new(cx),
                content_height,
            }
        }
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let content = div().w_full().h(px(self.content_height)).flex_shrink_0();
            // A fixed, bounded pane, so the content either overflows it or
            // does not depending purely on content_height.
            div().w(px(PANE)).h(px(PANE)).child(vertical_scroll(
                "page",
                &self.view,
                ScrollbarStyle::default(),
                content,
                cx,
            ))
        }
    }

    #[gpui::test]
    fn content_taller_than_the_pane_shows_a_vertical_scrollbar(cx: &mut TestAppContext) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(PANE * 3.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.view.scroll_state().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_some(),
            "content taller than the pane must show a vertical scrollbar without the caller \
             computing any height -- the axis measures the overflow itself"
        );
    }

    #[gpui::test]
    fn content_that_fits_the_pane_shows_no_scrollbar(cx: &mut TestAppContext) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(PANE / 2.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.view.scroll_state().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_none(),
            "content that fits the pane must render no scrollbar"
        );
    }
}
