//! The results pane's bottom connection/status bar.

use gpui::{Context, Div, SharedString, div, prelude::*, px, rgb};
use zsql_ui::grid;
use zsql_ui::theme::ActiveTheme;

use super::{ResultsView, format_total_row_count, status_indicator, status_metrics};
use crate::session::SessionState;
use crate::ui::theme;

impl ResultsView {
    /// The status bar's connection-lifecycle dot color, label, and trailing
    /// error text (if any), from the session's real state and liveness --
    /// never [`ResultsView::effective_state`]'s frozen tab snapshot. A tab
    /// frozen to an older, successful result must not keep showing
    /// "Connected" while the session itself is `Connecting` to a different
    /// target, has errored, or has gone unreachable; conversely the label
    /// and error text are always computed from the same state, so they can
    /// never disagree.
    ///
    /// [`SessionState::Connected`] alone is enough for the "Connected"
    /// label: every registered driver's `connect()` already performs a real
    /// pool connection plus a synchronous liveness check before resolving,
    /// so `Connected` already implies a verified-reachable connection.
    /// Waiting for the recurring probe's first [`LivenessState::Healthy`]
    /// result on top of that would only add a probe-interval-sized delay
    /// with no correctness benefit.
    pub(super) fn connection_status(
        &self,
        cx: &Context<Self>,
    ) -> (u32, &'static str, Option<String>) {
        let session = self.session.read(cx);
        let state = session.state();
        let liveness = session.liveness();
        let (dot_color, label) = status_indicator(state, liveness, cx.theme());
        let error_message = match state {
            SessionState::Error(message) => Some(message.clone()),
            _ => None,
        };
        (dot_color, label, error_message)
    }

    /// The bottom connection/status bar: connection state + label, row
    /// count, and elapsed query time.
    pub(super) fn render_status_bar(&self, cx: &Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let (dot_color, label, error_message) = self.connection_status(cx);

        // The left cluster (connection status, query metrics, any error
        // message) grows to fill the bar; the theme trigger is a fixed-width
        // sibling after it, so it always lands flush against the bar's right
        // edge regardless of which optional pieces the left cluster shows.
        let mut left = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(grid::status_dot(dot_color))
            .child(
                div()
                    .flex_shrink_0()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(colors.text_primary))
                    .child(label),
            );

        // Query metrics (row/line count, elapsed time) keep coming from the
        // displayed tab's own effective state, frozen or live: a frozen
        // tab's completed-query numbers are unrelated to the session's
        // current connection lifecycle.
        let effective_state = self.effective_state(cx);
        let (metrics_count, metrics_unit) = match self.text_view.read(cx).line_count() {
            Some(lines) => (lines, "lines"),
            None => (self.effective_result(cx).rows.len(), "rows"),
        };
        if let Some((count_text, elapsed_text)) =
            status_metrics(effective_state, metrics_count, metrics_unit)
        {
            left = left.child(count_text).child(elapsed_text);
        }

        if let Some(total_row_count_text) =
            format_total_row_count(self.session.read(cx).row_count())
        {
            left = left.child(total_row_count_text);
        }

        if let Some(message) = error_message {
            left = left.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.status_error))
                    .child(message),
            );
        }

        let active_display_name: SharedString = self
            .appearance_modal
            .as_ref()
            .map_or_else(
                || crate::theme_resolve::display_name_for(zsql_ui::theme::ZSQL_DARK_NAME),
                |modal| modal.read(cx).active_theme_display_name(),
            )
            .into();

        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_4()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors.bg_panel))
            .border_t_1()
            .border_color(rgb(colors.border))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .text_color(rgb(colors.text_secondary))
            .child(left)
            .child(super::appearance_trigger::render_theme_trigger(
                self.appearance_modal.clone(),
                colors,
                active_display_name,
                cx,
            ))
    }
}
