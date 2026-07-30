//! The results header bar: source/count label, plus (always) the Grid|Text
//! view switch and, while Text is active, the copy control.

use gpui::{ClickEvent, Context, Div, Window, div, prelude::*, px, rgb};
use zsql_ui::button::ButtonSwitch;
use zsql_ui::theme::{ActiveTheme, Theme};

use super::{ResultsView, ViewMode, filtered_count_summary, results_bar_count_text};
use crate::ui::theme;

impl ResultsView {
    /// The results header bar: row/line count + source/relation label, plus
    /// (always) the Grid|Text view switch and, while Text is active, the
    /// copy control.
    pub(super) fn render_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let document_lines = self.text_view.read(cx).line_count();
        let filtered_summary = filtered_count_summary(self.preview.as_ref());
        let count_text = filtered_summary.as_ref().map_or_else(
            || {
                results_bar_count_text(
                    self.effective_state(cx),
                    self.effective_result(cx).rows.len(),
                    document_lines,
                )
            },
            |(filtered, _)| filtered.clone(),
        );

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_shrink_0()
            .gap_3()
            .h(theme::RESULTS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors.bg_panel))
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_baseline()
                            .gap_2()
                            .flex_shrink_0()
                            .text_size(px(theme::RESULTS_TAB_TEXT_SIZE))
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(colors.text_primary))
                                    .child("Results"),
                            )
                            .child(
                                div()
                                    .font_family(&cx.theme().fonts.data)
                                    .text_color(rgb(colors.accent))
                                    .child(count_text),
                            )
                            .children(filtered_summary.map(|(_, suffix)| {
                                div()
                                    .font_family(&cx.theme().fonts.data)
                                    .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                                    .text_color(rgb(colors.text_tertiary))
                                    .child(suffix)
                            })),
                    )
                    .child(
                        div()
                            .font_family(&cx.theme().fonts.data)
                            .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                            .text_color(rgb(colors.text_tertiary))
                            .min_w_0()
                            .truncate()
                            .child(self.source_label.clone()),
                    ),
            )
            .child(self.render_bar_right(window, cx))
    }

    /// The results bar's trailing controls: the copy button while
    /// Text is active, then the Grid|Text switch, which is always rendered
    /// regardless of the current result's shape.
    fn render_bar_right(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let active_theme = cx.theme();
        let switch_enabled = self.effective_result(cx).has_single_text_column();
        let displayed_row_count = self.effective_result(cx).rows.len();

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap(theme::RESULTS_BAR_RIGHT_GAP);

        row = row.child(super::pager::render_pager_bar(
            self.preview.as_ref(),
            displayed_row_count,
            active_theme,
        ));

        if self.view_mode == ViewMode::Text {
            row = row.child(Self::render_icon_button(
                "results-text-copy",
                "copy all",
                false,
                active_theme,
                cx.listener(|view, _: &ClickEvent, _window, cx| view.copy_text_document(cx, true)),
            ));
        }

        row.child(self.render_view_switch(window, cx, switch_enabled))
    }

    /// A small text button for the results bar's trailing controls,
    /// styled like the plain-text icon affordances elsewhere in the
    /// app. `active` paints it with the view switch's active-segment colors;
    /// the copy button, which has no on/off state, always passes `false`.
    fn render_icon_button(
        id: &'static str,
        label: &'static str,
        active: bool,
        active_theme: &Theme,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> gpui::Stateful<Div> {
        let colors = active_theme.colors;
        let mut button = div()
            .id(id)
            .cursor_pointer()
            .px(theme::RESULTS_ICON_BUTTON_PADDING_X)
            .py(theme::RESULTS_ICON_BUTTON_PADDING_Y)
            .rounded(px(theme::RESULTS_ICON_BUTTON_RADIUS))
            .text_size(px(theme::RESULTS_ICON_BUTTON_TEXT_SIZE))
            .child(label)
            .on_click(on_click);

        if active {
            button = button
                .bg(rgb(theme::view_switch_active_bg(active_theme)))
                .text_color(rgb(theme::view_switch_active_text(active_theme)));
        } else {
            button = button
                .text_color(rgb(colors.text_tertiary))
                .hover(|el| el.text_color(rgb(colors.text_secondary)));
        }
        button
    }

    /// The Grid|Text segmented view switch
    fn render_view_switch(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        text_enabled: bool,
    ) -> impl IntoElement {
        let grid_id = "results-view-grid";
        let text_id = "results-view-text";
        let selected = match self.view_mode {
            ViewMode::Grid => grid_id,
            ViewMode::Text => text_id,
        };

        ButtonSwitch::new()
            .selected(selected)
            .disabled(!text_enabled)
            .add_option(
                window,
                cx,
                grid_id,
                "grid",
                cx.listener(|view, _e, _w, cx| {
                    view.set_view_mode(ViewMode::Grid, cx);
                }),
            )
            .add_option(
                window,
                cx,
                text_id,
                "text",
                cx.listener(|view, _e, _w, cx| {
                    view.set_view_mode(ViewMode::Text, cx);
                }),
            )
    }
}
