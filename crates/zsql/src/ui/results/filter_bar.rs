//! The filter bar strip under the results bar: committed filter chips
//! reading exactly like the `WHERE` fragment they generate, the AND/OR
//! connector pills between them, the "+ filter" control (which opens a
//! column picker, then a chip editor for the picked column) and "clear all"
//! control, and a chip's operator menu while it is being edited. Frozen to
//! read-only whenever [`ResultsView::preview`] is `None`, mirroring how the
//! sort headers and pager already go inert for a detached tab.

use gpui::{
    Context, Div, Entity, Focusable, SharedString, Stateful, TextOverflow, Window, deferred, div,
    prelude::*, px, rgb,
};
use zsql_core::{
    ColumnMeta, FilterCondition, FilterConditionId, FilterConnector, FilterOperator,
    FilterValueRender,
};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState, TextFieldStyle};
use zsql_ui::theme::ActiveTheme;
use zsql_ui::utils::OnHoverState;

use super::ResultsView;
use super::pager::PreviewAction;
use crate::ui::theme as app_theme;

/// The filter bar's in-progress chip edit: a brand-new filter
/// (`editing_id` is `None`) or an existing committed condition being
/// changed (`editing_id` is `Some`). `column`/`type_name` never change once
/// the editor opens -- for a brand-new filter, the target column is picked
/// from [`ResultsView::begin_add_filter`]'s column picker before this editor
/// ever exists.
pub(crate) struct FilterEditorState {
    editing_id: Option<FilterConditionId>,
    column: String,
    type_name: String,
    operator: FilterOperator,
    value_field: Entity<TextFieldState>,
    menu_open: bool,
}

impl ResultsView {
    /// The filter bar strip: the "FILTER" label, every committed chip
    /// (joined by its own AND/OR connector pill), the in-progress editor
    /// chip if one is open, and the "+ filter"/"clear all" controls. Every
    /// interactive part is disabled while [`ResultsView::preview`] is
    /// `None` -- the active tab is not a live, unedited generated preview.
    pub(super) fn render_filter_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let active_theme = cx.theme().clone();
        let colors = active_theme.colors;
        let interactive = self.preview.is_some();
        let conditions: Vec<FilterCondition> = self
            .preview
            .as_ref()
            .map(|p| p.state.filters().conditions().to_vec())
            .unwrap_or_default();
        let connectors: Vec<FilterConnector> = self
            .preview
            .as_ref()
            .map(|p| p.state.filters().connectors().to_vec())
            .unwrap_or_default();
        let dispatch = self.preview.as_ref().map(|p| p.dispatch.clone());
        let columns: Vec<ColumnMeta> = self.effective_result(cx).columns.clone();

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .flex_shrink_0()
            .min_h(app_theme::FILTER_BAR_MIN_HEIGHT)
            .gap(app_theme::FILTER_BAR_GAP)
            .w_full()
            .px_3()
            .py_1()
            .bg(rgb(colors.bg_panel))
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .font_family(&active_theme.fonts.data)
            .text_size(px(app_theme::FILTER_BAR_TEXT_SIZE))
            .child(
                div()
                    .text_size(px(app_theme::PAGER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child("FILTER"),
            );

        for (index, condition) in conditions.iter().enumerate() {
            let is_editing_this = self
                .filter_editor
                .as_ref()
                .is_some_and(|e| e.editing_id == Some(condition.id()));
            if is_editing_this {
                bar = bar.child(self.render_filter_editor(&active_theme, cx));
            } else {
                bar = bar.child(Self::render_chip(
                    condition,
                    interactive,
                    &active_theme,
                    dispatch.clone(),
                    cx,
                ));
            }
            if let Some(&connector) = connectors.get(index) {
                bar = bar.child(Self::render_connector_pill(
                    index,
                    connector,
                    interactive,
                    &active_theme,
                    dispatch.clone(),
                ));
            }
        }

        // A brand-new (not-yet-committed) filter's editor renders after
        // every existing chip.
        if self
            .filter_editor
            .as_ref()
            .is_some_and(|e| e.editing_id.is_none())
        {
            bar = bar.child(self.render_filter_editor(&active_theme, cx));
        }

        bar = bar.child(self.render_add_filter_area(
            interactive,
            &columns,
            &active_theme,
            window,
            cx,
        ));

        if !conditions.is_empty() {
            bar = bar.child(Self::render_clear_all_control(
                interactive,
                &active_theme,
                dispatch,
                window,
                cx,
            ));
        }

        bar
    }

    /// One committed filter chip: column name, teal operator, and the
    /// value as [`FilterCondition::rendered_value`] classifies it -- a
    /// quoted string, a bare number, or (marked `fx`) a pass-through
    /// expression. Clicking the chip body opens it for editing while
    /// `interactive`; its own remove control always dispatches
    /// [`PreviewAction::RemoveFilter`] regardless of edit state.
    fn render_chip(
        condition: &FilterCondition,
        interactive: bool,
        active_theme: &zsql_ui::theme::Theme,
        dispatch: Option<super::pager::PreviewDispatch>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = active_theme.colors;
        let id = condition.id();

        let mut chip = div()
            .id(SharedString::from(format!("filter-chip-{id}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(app_theme::FILTER_CHIP_INNER_GAP)
            .h(app_theme::FILTER_CHIP_HEIGHT)
            .pl(app_theme::FILTER_CHIP_PADDING_X)
            .pr(app_theme::FILTER_CHIP_PADDING_RIGHT)
            .rounded(px(app_theme::FILTER_CHIP_RADIUS))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.bg_raised))
            .child(
                div()
                    .text_color(rgb(colors.text_primary))
                    .child(condition.column().to_owned()),
            )
            .child(
                div()
                    .text_color(rgb(colors.accent))
                    .child(condition.operator().as_sql_symbol()),
            )
            .child(Self::render_chip_value(condition, active_theme));

        if interactive {
            let condition = condition.clone();
            chip = chip
                .cursor_pointer()
                .on_click(cx.listener(move |view, _event, window, cx| {
                    view.begin_edit_filter(&condition, window, cx);
                }));
        }

        let icon = icon(
            IconName::Close,
            app_theme::FILTER_CHIP_REMOVE_ICON_SIZE,
            colors.text_tertiary,
        );

        let remove = div()
            .id(SharedString::from(format!("filter-chip-remove-{id}")))
            .group(format!("filter-chip-remove-{id}"))
            .w(app_theme::FILTER_CHIP_REMOVE_SIZE)
            .h(app_theme::FILTER_CHIP_REMOVE_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(app_theme::FILTER_CHIP_REMOVE_RADIUS))
            .text_color(rgb(colors.text_tertiary));

        let remove = if let Some(dispatch) = dispatch {
            remove
                .child(icon.group_hover(format!("filter-chip-remove-{id}"), |el| {
                    el.text_color(rgb(colors.status_error))
                }))
                .cursor_pointer()
                .hover(|el| el.text_color(rgb(colors.status_error)))
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    dispatch(PreviewAction::RemoveFilter(id), cx);
                })
        } else {
            remove
                .child(icon)
                .opacity(app_theme::PAGER_DISABLED_OPACITY)
        };

        chip.child(remove)
    }

    /// A chip's value span: quoted/escaped string or bare number in their
    /// own colors, or (for an expression) the raw text plus a small `fx`
    /// tag marking it as passed through unquoted.
    fn render_chip_value(condition: &FilterCondition, active_theme: &zsql_ui::theme::Theme) -> Div {
        let colors = active_theme.colors;
        match condition.rendered_value() {
            FilterValueRender::Expression(text) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(app_theme::FILTER_VALUE_EXPRESSION_GAP)
                .child(div().text_color(rgb(colors.accent)).child(text))
                .child(
                    div()
                        .text_size(px(app_theme::FILTER_FX_TAG_TEXT_SIZE))
                        .px(app_theme::FILTER_FX_TAG_PADDING_X)
                        .rounded(px(app_theme::FILTER_FX_TAG_RADIUS))
                        .border_1()
                        .border_color(colors.accent_outline())
                        .text_color(rgb(colors.accent))
                        .child("fx"),
                ),
            FilterValueRender::Literal(text) => {
                let is_numeric = !text.starts_with('\'');
                let color = if is_numeric {
                    colors.value_number
                } else {
                    colors.syntax_string
                };
                div().text_color(rgb(color)).child(text)
            }
        }
    }

    /// The AND/OR connector pill joining `conditions()[index]` to
    /// `conditions()[index + 1]`: clicking it toggles the connector while
    /// `interactive`.
    fn render_connector_pill(
        index: usize,
        connector: FilterConnector,
        interactive: bool,
        active_theme: &zsql_ui::theme::Theme,
        dispatch: Option<super::pager::PreviewDispatch>,
    ) -> Stateful<Div> {
        let colors = active_theme.colors;
        let label = connector.as_sql();
        let text_color = match connector {
            FilterConnector::And => colors.text_tertiary,
            FilterConnector::Or => colors.status_warn,
        };

        let pill = div()
            .id(SharedString::from(format!("filter-connector-{index}")))
            .flex()
            .items_center()
            .h(app_theme::FILTER_CONNECTOR_HEIGHT)
            .px(app_theme::FILTER_CONNECTOR_PADDING_X)
            .rounded(px(app_theme::FILTER_CONNECTOR_RADIUS))
            .text_color(rgb(text_color))
            .child(label);

        match (interactive, dispatch) {
            (true, Some(dispatch)) => pill
                .cursor_pointer()
                .border_1()
                .border_color(rgb(colors.border))
                .hover(|el| el.bg(rgb(colors.bg_raised)))
                .on_click(move |_event, _window, cx| {
                    dispatch(PreviewAction::ToggleFilterConnector(index), cx);
                }),
            _ => pill.opacity(app_theme::PAGER_DISABLED_OPACITY),
        }
    }

    /// The "+ filter" control plus, while [`ResultsView::filter_column_picker`]
    /// is open, the column picker dropdown anchored below it: picking any
    /// column of the current result opens a fresh filter editor targeting
    /// that column.
    fn render_add_filter_area(
        &self,
        interactive: bool,
        columns: &[ColumnMeta],
        active_theme: &zsql_ui::theme::Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut wrap = div().relative().child(Self::render_add_filter_control(
            interactive,
            !columns.is_empty(),
            active_theme,
            window,
            cx,
        ));
        if self.filter_column_picker_open {
            wrap = wrap.child(Self::render_column_picker(
                columns,
                active_theme,
                window,
                cx,
            ));
        }
        wrap
    }

    /// The "+ filter" control: a no-op while not `interactive` or the
    /// active result has no columns to filter.
    fn render_add_filter_control(
        interactive: bool,
        has_column: bool,
        active_theme: &zsql_ui::theme::Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = active_theme.colors;
        let control = div()
            .id("filter-add")
            .flex()
            .items_center()
            .gap(app_theme::FILTER_ADD_CONTROL_GAP)
            .h(app_theme::FILTER_CONTROL_HEIGHT)
            .px(app_theme::FILTER_ADD_PADDING_X)
            .rounded(px(app_theme::FILTER_ADD_RADIUS))
            .border_1()
            .border_dashed()
            .border_color(rgb(colors.border))
            .text_color(rgb(colors.text_tertiary))
            .child(div().text_color(rgb(colors.accent)).child("+"))
            .child("filter");

        if interactive && has_column {
            control
                .cursor_pointer()
                .on_hover_state(window, cx, |el| el.text_color(rgb(colors.text_secondary)))
                .on_click(cx.listener(|view, _event, window, cx| {
                    view.begin_add_filter(window, cx);
                }))
        } else {
            control.opacity(app_theme::PAGER_DISABLED_OPACITY)
        }
    }

    /// The column picker dropdown: one entry per column of the current
    /// result, each opening a fresh filter editor targeting it
    fn render_column_picker(
        columns: &[ColumnMeta],
        active_theme: &zsql_ui::theme::Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = active_theme.colors;
        let mut menu = div()
            .absolute()
            .top(app_theme::FILTER_MENU_TOP_OFFSET)
            .left(px(0.0))
            .w(app_theme::FILTER_OP_MENU_WIDTH)
            .p(app_theme::FILTER_OP_MENU_PADDING)
            .rounded(px(app_theme::FILTER_OP_MENU_RADIUS))
            .border_1()
            .border_color(rgb(colors.border))
            .block_mouse_except_scroll()
            .bg(rgb(colors.bg_overlay))
            .shadow_lg();

        let overlay = div()
            .id("filter-column-picker-overlay")
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .block_mouse_except_scroll()
            .on_click(cx.listener(|view, _event, _window, cx| {
                view.filter_column_picker_open = false;
                cx.notify();
            }));

        for column in columns {
            let column = column.clone();
            let item = div()
                .id(SharedString::from(format!(
                    "filter-column-menu-{}",
                    column.name
                )))
                .cursor_pointer()
                .flex()
                .flex_row()
                .items_center()
                .h(app_theme::FILTER_OP_MENU_ITEM_HEIGHT)
                .px(app_theme::FILTER_OP_MENU_ITEM_PADDING_X)
                .rounded(px(app_theme::FILTER_OP_MENU_ITEM_RADIUS))
                .hover(|el| el.bg(colors.accent_wash()))
                .text_color(rgb(colors.text_primary))
                .child(
                    div()
                        .max_w_full()
                        .overflow_x_hidden()
                        .text_overflow(TextOverflow::Truncate("…".into()))
                        .child(column.name.clone()),
                )
                .child(
                    div()
                        .ml_auto()
                        .text_color(rgb(colors.text_tertiary))
                        .child(column.type_name.clone()),
                )
                .on_click(cx.listener(move |view, _event, window, cx| {
                    view.pick_filter_column(&column, window, cx);
                }));
            menu = menu.child(item);
        }

        deferred(div().relative().child(overlay).child(menu))
    }

    /// The "clear all" control, shown only while at least one filter is
    /// committed: a no-op while not `interactive`.
    fn render_clear_all_control(
        interactive: bool,
        active_theme: &zsql_ui::theme::Theme,
        dispatch: Option<super::pager::PreviewDispatch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = active_theme.colors;
        let control = div()
            .id("filter-clear-all")
            .flex()
            .items_center()
            .h(app_theme::FILTER_CONTROL_HEIGHT)
            .px(app_theme::FILTER_ADD_PADDING_X)
            .rounded(px(app_theme::FILTER_CLEAR_ALL_RADIUS))
            .text_size(px(app_theme::PAGER_TEXT_SIZE))
            .text_color(rgb(colors.text_tertiary))
            .child("clear all");

        match (interactive, dispatch) {
            (true, Some(dispatch)) => control
                .cursor_pointer()
                .on_hover_state(window, cx, |el| el.text_color(rgb(colors.status_error)))
                .on_click(move |_event, _window, cx| {
                    dispatch(PreviewAction::ClearFilters, cx);
                }),
            _ => control.opacity(app_theme::PAGER_DISABLED_OPACITY),
        }
    }

    /// The in-progress editor chip: the target column (fixed for the life
    /// of the edit), a clickable operator badge that opens
    /// [`FilterEditorState::menu_open`]'s v0 operator menu, and the value
    /// text field. Enter in the value field commits; the small `x` cancels
    /// without committing.
    fn render_filter_editor(
        &self,
        active_theme: &zsql_ui::theme::Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let colors = active_theme.colors;
        let Some(editor) = self.filter_editor.as_ref() else {
            return div().id("filter-editor-empty");
        };

        let op_id = SharedString::from("filter-editor-operator");
        let mut chip = div()
            .id("filter-editor")
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(app_theme::FILTER_CHIP_INNER_GAP)
            .h(app_theme::FILTER_CHIP_HEIGHT)
            .px(app_theme::FILTER_CHIP_PADDING_X)
            .rounded(px(app_theme::FILTER_CHIP_RADIUS))
            .border_1()
            .border_color(rgb(colors.accent))
            .bg(colors.accent_wash_soft())
            .child(
                div()
                    .text_color(rgb(colors.text_primary))
                    .child(editor.column.clone()),
            )
            .child(
                div()
                    .id(op_id)
                    .cursor_pointer()
                    .text_color(rgb(colors.accent))
                    .child(editor.operator.as_sql_symbol())
                    .on_click(cx.listener(|view, _event, _window, cx| {
                        view.toggle_filter_editor_menu(cx);
                    })),
            )
            .child(
                div()
                    .w(app_theme::FILTER_VALUE_FIELD_WIDTH)
                    .child(editor.value_field.clone()),
            )
            .child(
                div()
                    .id("filter-editor-cancel")
                    .cursor_pointer()
                    .text_color(rgb(colors.text_tertiary))
                    .hover(|el| el.text_color(rgb(colors.status_error)))
                    .child("\u{d7}")
                    .on_click(cx.listener(|view, _event, _window, cx| {
                        view.cancel_filter_edit(cx);
                    })),
            );

        if editor.menu_open {
            chip = chip.child(Self::render_operator_menu(active_theme, cx));
        }

        chip
    }

    /// The operator menu, documenting the v0 set exactly (no other operator
    /// is reachable from it): `like`/`ilike` carry their pattern hints.
    fn render_operator_menu(
        active_theme: &zsql_ui::theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = active_theme.colors;
        let mut menu = div()
            .absolute()
            .top(app_theme::FILTER_MENU_TOP_OFFSET)
            .left(px(0.0))
            .w(app_theme::FILTER_OP_MENU_WIDTH)
            .p(app_theme::FILTER_OP_MENU_PADDING)
            .rounded(px(app_theme::FILTER_OP_MENU_RADIUS))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.bg_overlay))
            .block_mouse_except_scroll()
            .shadow_lg();

        for operator in FilterOperator::ALL {
            let hint = operator.pattern_hint();
            let mut item = div()
                .id(SharedString::from(format!(
                    "filter-op-menu-{}",
                    operator.as_sql_symbol()
                )))
                .cursor_pointer()
                .flex()
                .flex_row()
                .items_center()
                .h(app_theme::FILTER_OP_MENU_ITEM_HEIGHT)
                .px(app_theme::FILTER_OP_MENU_ITEM_PADDING_X)
                .rounded(px(app_theme::FILTER_OP_MENU_ITEM_RADIUS))
                .hover(|el| el.bg(colors.accent_wash()))
                .text_color(rgb(colors.text_primary))
                .child(
                    div()
                        .w(app_theme::FILTER_OP_MENU_SYMBOL_WIDTH)
                        .flex_shrink_0()
                        .text_color(rgb(colors.accent))
                        .child(operator.as_sql_symbol()),
                )
                .on_click(cx.listener(move |view, _event, _window, cx| {
                    view.set_filter_editor_operator(operator, cx);
                }));
            if let Some(hint) = hint {
                item = item.child(
                    div()
                        .ml_auto()
                        .text_color(rgb(colors.text_tertiary))
                        .child(hint),
                );
            }
            menu = menu.child(item);
        }

        deferred(menu)
    }

    /// Toggle the column picker open, so the next click picks which column
    /// the new filter targets (see [`ResultsView::pick_filter_column`]). A
    /// no-op while not [`ResultsView::preview`]-active or the result has no
    /// columns.
    pub(super) fn begin_add_filter(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.is_none() || self.effective_result(cx).columns.is_empty() {
            return;
        }
        self.filter_editor = None;
        self.filter_column_picker_open = !self.filter_column_picker_open;
        cx.notify();
    }

    /// Open a fresh filter editor targeting `column`, defaulting to `=`,
    /// and close the column picker. A no-op while not
    /// [`ResultsView::preview`]-active.
    pub(super) fn pick_filter_column(
        &mut self,
        column: &ColumnMeta,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_column_picker_open = false;
        if self.preview.is_none() {
            cx.notify();
            return;
        }
        self.open_filter_editor(
            None,
            column.name.clone(),
            column.type_name.clone(),
            FilterOperator::Eq,
            "",
            window,
            cx,
        );
    }

    /// Open a filter editor for an existing committed condition, prefilled
    /// with its current operator/value. A no-op while not
    /// [`ResultsView::preview`]-active.
    pub(super) fn begin_edit_filter(
        &mut self,
        condition: &FilterCondition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preview.is_none() {
            return;
        }
        self.filter_column_picker_open = false;
        self.open_filter_editor(
            Some(condition.id()),
            condition.column().to_owned(),
            condition.type_name().to_owned(),
            condition.operator(),
            condition.value(),
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_filter_editor(
        &mut self,
        editing_id: Option<FilterConditionId>,
        column: String,
        type_name: String,
        operator: FilterOperator,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value_field = cx.new(|cx| {
            TextFieldState::new("value", Some(value), cx).style(TextFieldStyle {
                height: app_theme::FILTER_CHIP_HEIGHT,
                padding_y: px(0.0),
                border_w: px(0.0),
                ..Default::default()
            })
        });
        cx.subscribe(&value_field, |view, _field, event, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                view.commit_filter_edit(cx);
            }
        })
        .detach();
        let focus_handle = value_field.read(cx).focus_handle(cx);
        window.focus(&focus_handle);

        self.filter_editor = Some(FilterEditorState {
            editing_id,
            column,
            type_name,
            operator,
            value_field,
            menu_open: false,
        });
        cx.notify();
    }

    /// Close the in-progress filter editor without committing.
    pub(super) fn cancel_filter_edit(&mut self, cx: &mut Context<Self>) {
        if self.filter_editor.take().is_some() {
            cx.notify();
        }
    }

    /// Show/hide the in-progress editor's operator menu.
    pub(super) fn toggle_filter_editor_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.filter_editor {
            editor.menu_open = !editor.menu_open;
            cx.notify();
        }
    }

    /// Pick `operator` for the in-progress editor and close the menu.
    pub(super) fn set_filter_editor_operator(
        &mut self,
        operator: FilterOperator,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.filter_editor {
            editor.operator = operator;
            editor.menu_open = false;
            cx.notify();
        }
    }

    /// Commit the in-progress editor: dispatches
    /// [`PreviewAction::AddFilter`] for a brand-new filter or
    /// [`PreviewAction::UpdateFilter`] for an edit of an existing one, then
    /// closes the editor. A no-op (but still closes the editor) if
    /// [`ResultsView::preview`] went away while the editor was open.
    pub(super) fn commit_filter_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.filter_editor.take() else {
            return;
        };
        let value = editor.value_field.read(cx).value().to_string();
        if let Some(preview) = &self.preview {
            let dispatch = preview.dispatch.clone();
            let action = match editor.editing_id {
                None => PreviewAction::AddFilter {
                    column: editor.column,
                    type_name: editor.type_name,
                    operator: editor.operator,
                    value,
                },
                Some(id) => PreviewAction::UpdateFilter {
                    id,
                    operator: editor.operator,
                    value,
                },
            };
            dispatch(action, cx);
        }
        cx.notify();
    }

    /// Whether the filter bar currently has an in-progress chip edit open.
    #[cfg(test)]
    pub(crate) fn filter_editor_is_open_for_test(&self) -> bool {
        self.filter_editor.is_some()
    }

    /// Whether the "+ filter" column picker is currently open.
    #[cfg(test)]
    pub(crate) fn filter_column_picker_is_open_for_test(&self) -> bool {
        self.filter_column_picker_open
    }

    /// The in-progress editor's current operator, if one is open.
    #[cfg(test)]
    pub(crate) fn filter_editor_operator_for_test(&self) -> Option<FilterOperator> {
        self.filter_editor.as_ref().map(|e| e.operator)
    }

    /// The in-progress editor's current value field text, if one is open.
    #[cfg(test)]
    pub(crate) fn filter_editor_value_for_test(&self, cx: &gpui::App) -> Option<String> {
        self.filter_editor
            .as_ref()
            .map(|e| e.value_field.read(cx).value().to_string())
    }

    /// Whether the in-progress editor's operator menu is open.
    #[cfg(test)]
    pub(crate) fn filter_editor_menu_open_for_test(&self) -> bool {
        self.filter_editor.as_ref().is_some_and(|e| e.menu_open)
    }

    /// Set the in-progress editor's value field text directly, standing in
    /// for simulated keystrokes.
    #[cfg(test)]
    pub(crate) fn set_filter_editor_value_for_test(&mut self, value: &str, cx: &mut Context<Self>) {
        if let Some(editor) = &self.filter_editor {
            let field = editor.value_field.clone();
            field.update(cx, |field, cx| field.set_value(value, cx));
        }
    }
}
