//! The editor's own find bar: pins `zsql_ui::quick_find_bar::QuickFindBar`
//! to [`crate::find::EditorFind`]'s pure match/navigation core, and reacts
//! to the bar's events by moving the buffer's cursor to whichever span
//! becomes current.

use gpui::{AnyElement, Context, Entity, Subscription, Window, div, prelude::*};
use zsql_ui::quick_find_bar::{QuickFindBar, QuickFindBarEvent};

use super::{CloseFind, EditorView, FindNext, FindPrev, OpenFind};
use crate::Position;
use crate::find::{EditorFind, MatchSpan};
use crate::theme;

/// The key context the find bar's own next/prev/close bindings are scoped
/// to, active whenever the bar (or its query input) holds window focus.
pub const KEY_CONTEXT: &str = "EditorFind";

/// The open find session: the pure match/navigation core plus the bar
/// overlay entity. Dropping it closes the session and its event
/// subscription with it.
pub(super) struct EditorFindState {
    core: EditorFind,
    bar: Entity<QuickFindBar>,
    _bar_events: Subscription,
}

impl EditorFindState {
    /// Open a fresh session: an empty core plus a new bar whose events are
    /// routed to the hosting view.
    fn open(window: &Window, cx: &mut Context<EditorView>) -> Self {
        let bar = cx.new(|cx| QuickFindBar::new("editor-find-bar", "Find...", cx));
        let bar_events = cx.subscribe_in(&bar, window, EditorView::handle_find_event);
        Self {
            core: EditorFind::new(),
            bar,
            _bar_events: bar_events,
        }
    }

    /// The bar overlay entity, for rendering and input focus.
    pub(super) fn bar(&self) -> &Entity<QuickFindBar> {
        &self.bar
    }

    /// Every matching span, in document order.
    pub(super) fn matches(&self) -> &[MatchSpan] {
        self.core.matches()
    }

    /// The current match's span, if any.
    pub(super) fn current(&self) -> Option<MatchSpan> {
        self.core.current_match()
    }

    /// The current match's 1-based position among [`EditorFindState::matches`].
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn current_number(&self) -> Option<usize> {
        self.core.current_number()
    }

    /// Push the core's current position and case mode into the bar's
    /// display. Call after any mutation the bar should reflect.
    fn push_status(&self, cx: &mut gpui::App) {
        let current = self.core.current_number().unwrap_or(0);
        let total = self.core.match_count();
        let case_on = self.core.case_sensitive();
        self.bar.update(cx, |bar, cx| {
            bar.set_status(current, total, case_on, cx);
        });
    }
}

// ---- the hosting view's own find wiring -----------------------------

impl EditorView {
    /// [`OpenFind`]'s handler: open the bar over the editor pane, focusing
    /// its query input, or refocus it if already open.
    #[tracing::instrument(name = "editor_open_find", skip_all)]
    pub(super) fn open_find(&mut self, _: &OpenFind, window: &mut Window, cx: &mut Context<Self>) {
        if self.compact {
            return;
        }
        if let Some(state) = &self.find {
            state.bar().read(cx).focus_input(window, cx);
            return;
        }
        let state = EditorFindState::open(window, cx);
        state.bar().read(cx).focus_input(window, cx);
        self.find = Some(state);
        tracing::debug!("opened the editor find bar");
        cx.notify();
    }

    /// [`CloseFind`]'s handler: close the bar and clear every highlight,
    /// leaving the buffer's cursor at the last current match (find doubles
    /// as jump-to), and return window focus to the editor pane's own focus
    /// handle.
    #[tracing::instrument(name = "editor_close_find", skip_all)]
    pub(super) fn close_find(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find.take().is_none() {
            return;
        }
        window.focus(&self.focus_handle);
        tracing::debug!("closed the editor find bar");
        cx.notify();
    }

    pub(super) fn find_next(&mut self, _: &FindNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.find_step(true, cx);
    }

    pub(super) fn find_prev(&mut self, _: &FindPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.find_step(false, cx);
    }

    /// The single reaction point for everything the bar asks for, whether
    /// requested by one of its buttons, submitting its input, or typing in
    /// it.
    fn handle_find_event(
        &mut self,
        _bar: &Entity<QuickFindBar>,
        event: &QuickFindBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            QuickFindBarEvent::QueryChanged(query) => self.set_find_query(query.clone(), cx),
            QuickFindBarEvent::StepRequested { forward } => self.find_step(*forward, cx),
            QuickFindBarEvent::CaseToggleRequested => self.toggle_find_case(cx),
            QuickFindBarEvent::DismissRequested => {
                self.close_find(&CloseFind, window, cx);
            }
        }
    }

    /// Step the current match forward (`forward`) or backward, wrapping at
    /// either end, and move the buffer's cursor to it.
    fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(state) = &mut self.find else {
            return;
        };
        let landed = if forward {
            state.core.next_match()
        } else {
            state.core.prev_match()
        };
        state.push_status(cx);
        self.land_on_match(landed, cx);
    }

    /// Flip case-sensitive matching, recompute against the buffer's current
    /// text, and move the cursor to whichever match is now current.
    fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.find.as_mut() else {
            return;
        };
        let case_sensitive = !state.core.case_sensitive();
        state
            .core
            .set_case_sensitive(case_sensitive, self.buffer.lines());
        let landed = state.core.current_match();
        state.push_status(cx);
        self.land_on_match(landed, cx);
    }

    /// Recompute matches for `query` against the buffer's current text and
    /// jump to the new current match, if any.
    fn set_find_query(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(state) = self.find.as_mut() else {
            return;
        };
        state.core.set_query(query, self.buffer.lines());
        let landed = state.core.current_match();
        state.push_status(cx);
        self.land_on_match(landed, cx);
    }

    /// Recompute the open bar's matches against the buffer's current text,
    /// keeping the current match on the same span where it is still one,
    /// without moving the cursor. Call this after a manual edit. A no-op
    /// while the bar is closed.
    pub(super) fn sync_find_matches(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.find.as_mut() else {
            return;
        };
        state.core.sync(self.buffer.lines());
        state.push_status(cx);
    }

    /// Move the buffer's cursor to `span`'s start, without creating or
    /// replacing a real selection, so `RunQuery`'s "selection if present,
    /// else whole buffer" behavior is never affected by find navigation.
    fn land_on_match(&mut self, span: Option<MatchSpan>, cx: &mut Context<Self>) {
        if let Some(span) = span {
            self.buffer.set_cursor(Position::new(span.line, span.start));
        }
        cx.notify();
    }

    /// The floating find overlay over the editor pane's top-right, wrapping
    /// the bar with its own key context and action handlers. `None` while
    /// the bar is closed.
    pub(super) fn render_find_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.find.as_ref()?;
        Some(
            div()
                .key_context(KEY_CONTEXT)
                .absolute()
                .top(theme::FIND_BAR_TOP_OFFSET)
                .right(theme::FIND_BAR_RIGHT_OFFSET)
                .on_action(cx.listener(Self::find_next))
                .on_action(cx.listener(Self::find_prev))
                .on_action(cx.listener(Self::close_find))
                .occlude()
                .child(state.bar().clone())
                .into_any_element(),
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
impl EditorView {
    /// Whether the find bar is currently open.
    #[must_use]
    pub fn find_is_open_for_test(&self) -> bool {
        self.find.is_some()
    }

    /// The find bar's query input focus handle, `None` while the bar is
    /// closed.
    #[must_use]
    pub fn find_input_focus_handle_for_test(&self, cx: &gpui::App) -> Option<gpui::FocusHandle> {
        self.find
            .as_ref()
            .map(|state| state.bar().read(cx).input_focus_handle(cx))
    }

    /// The open bar's total match count, `None` while the bar is closed.
    #[must_use]
    pub fn find_match_count_for_test(&self) -> Option<usize> {
        self.find.as_ref().map(|state| state.matches().len())
    }

    /// The open bar's current match's 1-based position, `None` while the
    /// bar is closed or it has no matches.
    #[must_use]
    pub fn find_current_number_for_test(&self) -> Option<usize> {
        self.find.as_ref()?.current_number()
    }

    /// Whether the open bar's matching is case-sensitive, `None` while the
    /// bar is closed.
    #[must_use]
    pub fn find_case_sensitive_for_test(&self) -> Option<bool> {
        self.find.as_ref().map(|state| state.core.case_sensitive())
    }
}
