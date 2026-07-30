//! The results pane's bottom connection/status bar.

use gpui::{Context, Div, SharedString, div, prelude::*, px, rgb};
use zsql_ui::grid;
use zsql_ui::theme::ActiveTheme;

use super::{ResultsView, format_total_row_count, status_indicator, status_metrics};
use crate::session::SessionState;
use crate::ui::theme;

impl ResultsView {
    /// Test-only mirror of [`ResultsView::status_bar_total_row_count_text`].
    #[cfg(test)]
    pub(crate) fn status_bar_total_row_count_text_for_test(&self) -> Option<String> {
        self.status_bar_total_row_count_text()
    }
}
