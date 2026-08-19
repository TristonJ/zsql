//! The results grid's per-cell edit popover: opening it on an edit-eligible
//! cell's double-click or F2, its literal/expression/NULL mode, and staging
//! the edit into the shared staged-changes queue.

use gpui::{
    App, Bounds, Context, Entity, EventEmitter, Focusable as _, MouseDownEvent, Pixels, Point,
    Render, Window, div, prelude::*,
};
use zsql_core::{
    ColumnMeta, FilterValueRender, Value, classify_filter_value, render_literal_value,
};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState, TextFieldStyle};

use super::edit_popover::CellEditPopover;
use super::{EditCell, ResultsView};
use crate::staging::UpdateValue;
use crate::ui::format::{ValueKind, format_value};
use crate::ui::theme as app_theme;

/// The key context the edit popover's own key bindings (currently just
/// Escape) are scoped to, so they only fire while the popover is open and
/// its input holds focus.
pub const KEY_CONTEXT: &str = "CellEditPopover";

/// Which mode a [`CellEditState`] currently renders and stages under: a
/// quoted-or-bare literal, a raw expression, or SQL NULL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellEditMode {
    Literal,
    Expression,
    Null,
}

/// The open cell edit popover's state: which cell it targets, its input,
/// and its current mode.
struct CellEditState {
    row: usize,
    col: usize,
    column: String,
    type_name: String,
    was_text: String,
    input: Entity<TextFieldState>,
    mode: CellEditMode,
    /// Whether `mode` was pinned by an explicit chip click rather than the
    /// input's own live auto-classification. Once pinned, further typing
    /// leaves `mode` alone: an override always wins over auto-detection.
    mode_pinned: bool,
    /// Where the popover anchors, computed once when it opened. `None` when
    /// neither an explicit click position nor the cell's own painted bounds
    /// were available at that moment.
    anchor: Option<Point<Pixels>>,
    /// What the input held before NULL mode replaced it with the word
    /// "NULL", restored if the edit switches back to a typed mode.
    pre_null_text: String,
}

impl CellEditState {
    /// Build the state for editing `(row, col)`, prefilled from the cell's
    /// already-staged edit if one exists, otherwise from `original_value`.
    fn new(
        row: usize,
        col: usize,
        column: ColumnMeta,
        original_value: &Value,
        staged_value: Option<UpdateValue>,
        anchor: Option<Point<Pixels>>,
        cx: &mut App,
    ) -> Self {
        let was_text = was_value_text(original_value);
        let (initial_text, mode, mode_pinned) = match staged_value {
            Some(UpdateValue::Literal(text)) => (
                crate::staging::unquote_sql_string(&text),
                CellEditMode::Literal,
                true,
            ),
            Some(UpdateValue::Expression(text)) => (text, CellEditMode::Expression, true),
            Some(UpdateValue::Null) => (String::new(), CellEditMode::Null, true),
            None if matches!(original_value, Value::Null) => {
                (String::new(), CellEditMode::Null, false)
            }
            None => {
                let text = format_value(original_value).text;
                let mode = auto_mode(&text, &column.type_name);
                (text, mode, false)
            }
        };
        // Pinned NULL mode (a staged NULL, or later the chip) locks the
        // input on the word "NULL": the mode stores the absence, never the
        // input's text (see `pending_update_value`). The unpinned default
        // for a NULL-valued cell leaves the input free, so typing a
        // replacement can still auto-reclassify.
        let locked_null = mode == CellEditMode::Null && mode_pinned;
        let (shown_text, pre_null_text) = if locked_null {
            ("NULL".to_owned(), initial_text)
        } else {
            (initial_text, String::new())
        };
        let input = cx.new(|cx| {
            TextFieldState::new("value", Some(&shown_text), cx).style(TextFieldStyle {
                height: app_theme::EDIT_POPOVER_INPUT_HEIGHT,
                ..Default::default()
            })
        });
        if locked_null {
            input.update(cx, |field, cx| field.set_disabled(true, cx));
        }
        Self {
            row,
            col,
            column: column.name,
            type_name: column.type_name,
            was_text,
            input,
            mode,
            mode_pinned,
            anchor,
            pre_null_text,
        }
    }

    /// Re-run auto-classification for the current input text against the
    /// column type, unless the mode has been pinned by an explicit chip
    /// click.
    fn sync_mode(&mut self, cx: &App) {
        if self.mode_pinned {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        self.mode = auto_mode(&text, &self.type_name);
    }

    /// A mode chip click: pin the edit to `mode`, overriding whatever
    /// auto-classification would otherwise pick. Entering NULL mode locks
    /// the input on the word "NULL"; leaving it restores whatever the input
    /// held before.
    fn set_mode(&mut self, mode: CellEditMode, cx: &mut App) {
        let locked_null = self.mode == CellEditMode::Null && self.mode_pinned;
        if mode == CellEditMode::Null && !locked_null {
            self.pre_null_text = self.input.read(cx).value().to_string();
            self.input.update(cx, |field, cx| {
                field.set_value("NULL", cx);
                field.set_disabled(true, cx);
            });
        } else if mode != CellEditMode::Null && self.mode == CellEditMode::Null {
            let restored = std::mem::take(&mut self.pre_null_text);
            self.input.update(cx, |field, cx| {
                field.set_disabled(false, cx);
                field.set_value(&restored, cx);
            });
        }
        self.mode = mode;
        self.mode_pinned = true;
    }

    /// The current input/mode, rendered as the [`UpdateValue`] it would
    /// stage right now.
    fn pending_update_value(&self, cx: &App) -> UpdateValue {
        match self.mode {
            CellEditMode::Null => UpdateValue::Null,
            CellEditMode::Expression => {
                UpdateValue::Expression(self.input.read(cx).value().trim().to_owned())
            }
            CellEditMode::Literal => UpdateValue::Literal(render_literal_value(
                &self.input.read(cx).value(),
                &self.type_name,
            )),
        }
    }
}

/// Emitted by [`CellEditor`] when its popover closes.
pub(super) enum CellEditorEvent {
    /// Enter staged the edit: the host should commit `value` against the
    /// edited cell.
    Staged {
        row: usize,
        col: usize,
        column: String,
        value: UpdateValue,
    },
    /// The popover closed without staging anything.
    Cancelled,
}

/// The cell edit popover, as its own entity; state is `None` while closed.
pub(super) struct CellEditor {
    state: Option<CellEditState>,
}

impl EventEmitter<CellEditorEvent> for CellEditor {}

impl CellEditor {
    pub fn new() -> Self {
        Self { state: None }
    }

    /// Open the popover for `(row, col)`, focusing its input.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        row: usize,
        col: usize,
        column: ColumnMeta,
        original_value: &Value,
        staged_value: Option<UpdateValue>,
        anchor: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(row, col, column = %column.name, "opened the cell edit popover");
        let state = CellEditState::new(row, col, column, original_value, staged_value, anchor, cx);
        cx.subscribe(&state.input, |editor, _field, event, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                editor.stage(cx);
            }
        })
        .detach();
        cx.observe(&state.input, |editor, _field, cx| {
            if let Some(state) = &mut editor.state {
                state.sync_mode(cx);
            }
            cx.notify();
        })
        .detach();
        window.focus(&state.input.read(cx).focus_handle(cx));
        self.state = Some(state);
        cx.notify();
    }

    /// Enter in the popover's input: close the popover and emit
    /// [`CellEditorEvent::Staged`] with the value it held.
    #[tracing::instrument(name = "cell_editor_stage", skip(self, cx))]
    fn stage(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.state.take() else {
            return;
        };
        let value = state.pending_update_value(cx);
        cx.emit(CellEditorEvent::Staged {
            row: state.row,
            col: state.col,
            column: state.column,
            value,
        });
        cx.notify();
    }

    /// Esc, or a click outside the popover: close it with no staging change
    /// and emit [`CellEditorEvent::Cancelled`].
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.state.take().is_some() {
            cx.emit(CellEditorEvent::Cancelled);
            cx.notify();
        }
    }

    /// Drop the popover without notifying the host, e.g. on a tab switch or
    /// query rerun that replaces the result it was editing.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.state.take().is_some() {
            cx.notify();
        }
    }

    fn set_mode(&mut self, mode: CellEditMode, cx: &mut Context<Self>) {
        if let Some(state) = &mut self.state {
            state.set_mode(mode, cx);
            cx.notify();
        }
    }
}

impl Render for CellEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(state) = &self.state else {
            return div().into_any_element();
        };
        let rendered =
            super::edit_popover::set_fragment_runs(&state.column, &state.pending_update_value(cx));
        let editor = cx.entity();
        CellEditPopover::new(
            state.column.clone(),
            state.type_name.clone(),
            state.was_text.clone(),
            state.input.clone(),
            state.mode,
            rendered,
            state.anchor,
        )
        .on_pick_literal(cx.listener(|editor, _event, _window, cx| {
            editor.set_mode(CellEditMode::Literal, cx);
        }))
        .on_pick_expression(cx.listener(|editor, _event, _window, cx| {
            editor.set_mode(CellEditMode::Expression, cx);
        }))
        .on_pick_null(cx.listener(|editor, _event, _window, cx| {
            editor.set_mode(CellEditMode::Null, cx);
        }))
        .on_stage(cx.listener(|editor, _event, _window, cx| editor.stage(cx)))
        .on_cancel(move |_window, cx| {
            editor.update(cx, CellEditor::cancel);
        })
        .into_any_element()
    }
}

impl ResultsView {
    /// Whether `(row, col)` may open the edit popover: a known relation with
    /// a usable primary key, the cell's row resolvable to an identity, and
    /// that row not already staged for deletion.
    pub(super) fn cell_edit_eligible(&self, cx: &App, row: usize, col: usize) -> bool {
        if !self.staging_available(cx) || self.effective_result(cx).columns.get(col).is_none() {
            return false;
        }
        let Some(identity) = self.row_identity_for(cx, row) else {
            return false;
        };
        self.staging
            .read(cx)
            .find_staged_delete(&identity)
            .is_none()
    }

    /// [`EditCell`]'s handler (F2 by default): open the popover for the
    /// grid's currently focused cell. A no-op with nothing selected.
    pub(super) fn edit_focused_cell(
        &mut self,
        _: &EditCell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.table_state.read(cx).focused_cell() else {
            return;
        };
        self.open_cell_edit(row, col, None, window, cx);
    }

    /// A data cell's double-click: opens the edit popover, anchored at the
    /// triggering click, on an edit-eligible cell, or falls back to
    /// [`ResultsView::open_value_panel_for`]'s existing behavior otherwise.
    #[tracing::instrument(
        name = "results_handle_cell_double_click",
        skip(self, event, window, cx)
    )]
    pub(super) fn handle_cell_double_click(
        &mut self,
        row: usize,
        col: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cell_edit_eligible(cx, row, col) {
            self.open_cell_edit(row, col, Some(event.position), window, cx);
        } else {
            self.open_value_panel_for(row, col, window, cx);
        }
    }

    /// Open the edit popover for `(row, col)`. A no-op when the cell is not
    /// edit-eligible.
    ///
    /// `click_position` anchors the popover at an explicit window position
    /// (the triggering double-click); `None` (the F2 entry point) anchors it
    /// at the focused cell's own last-painted bounds instead.
    #[tracing::instrument(
        name = "results_open_cell_edit",
        skip(self, click_position, window, cx)
    )]
    pub(super) fn open_cell_edit(
        &mut self,
        row: usize,
        col: usize,
        click_position: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cell_edit_eligible(cx, row, col) {
            tracing::trace!(
                row,
                col,
                "cell edit requested for an ineligible cell; ignored"
            );
            return;
        }
        let Some(column) = self.effective_result(cx).columns.get(col).cloned() else {
            return;
        };
        let Some(identity) = self.row_identity_for(cx, row) else {
            return;
        };
        let original_value = self
            .effective_result(cx)
            .rows
            .get(row)
            .and_then(|r| r.0.get(col))
            .cloned()
            .unwrap_or(Value::Null);
        let staged_value = self
            .staging
            .read(cx)
            .staged_update_value(&identity, &column.name);

        let anchor = click_position
            .or_else(|| {
                self.focused_cell_bounds
                    .get()
                    .map(|bounds: Bounds<Pixels>| {
                        Point::new(bounds.origin.x, bounds.origin.y + bounds.size.height)
                    })
            })
            .map(|point| Point::new(point.x, point.y + app_theme::EDIT_POPOVER_ANCHOR_GAP_Y));

        self.cell_editor.update(cx, |editor, cx| {
            editor.open(
                row,
                col,
                column,
                &original_value,
                staged_value,
                anchor,
                window,
                cx,
            );
        });
    }

    /// React to the popover closing: commit a staged value into the shared
    /// queue, and return keyboard focus to the grid on the same cell.
    pub(super) fn handle_cell_editor_event(
        &mut self,
        event: &CellEditorEvent,
        cx: &mut Context<Self>,
    ) {
        if let CellEditorEvent::Staged {
            row,
            col,
            column,
            value,
        } = event
        {
            match self.row_identity_for(cx, *row) {
                Some(identity) => {
                    let staged = self.staging.update(cx, |staging, cx| {
                        staging.stage_update(*row, identity, column.clone(), value.clone(), cx)
                    });
                    if !staged {
                        tracing::trace!(
                            row,
                            col,
                            "cell edit rejected; row now carries a staged delete"
                        );
                    }
                }
                None => {
                    tracing::trace!(
                        "staged cell edit's row no longer resolves to an identity; dropped"
                    );
                }
            }
        }
        self.pending_grid_refocus = true;
        cx.notify();
    }
}

/// The default mode `raw` auto-classifies to against `type_name`, per
/// [`classify_filter_value`]: NULL is never auto-selected here (only an
/// explicit chip click pins that mode), so everything else is a plain
/// literal or an expression.
fn auto_mode(raw: &str, type_name: &str) -> CellEditMode {
    match classify_filter_value(raw, type_name) {
        FilterValueRender::Expression(_) => CellEditMode::Expression,
        FilterValueRender::Literal(_) => CellEditMode::Literal,
    }
}

/// `value`'s display text for the popover header's "was <value>" hint:
/// single-quoted for a text-kind value (matching how it would render
/// quoted in generated SQL), bare for everything else (numbers, NULL, JSON,
/// timestamps, ...).
fn was_value_text(value: &Value) -> String {
    let formatted = format_value(value);
    if formatted.kind == ValueKind::Text {
        format!("'{}'", formatted.text)
    } else {
        formatted.text
    }
}

#[cfg(test)]
impl ResultsView {
    pub(crate) fn cell_edit_is_open_for_test(&self, cx: &App) -> bool {
        self.cell_editor.read(cx).state.is_some()
    }

    pub(crate) fn cell_edit_mode_for_test(&self, cx: &App) -> Option<CellEditMode> {
        self.cell_editor
            .read(cx)
            .state
            .as_ref()
            .map(|state| state.mode)
    }

    pub(crate) fn cell_edit_input_value_for_test(&self, cx: &App) -> Option<String> {
        self.cell_editor
            .read(cx)
            .state
            .as_ref()
            .map(|state| state.input.read(cx).value().to_string())
    }

    pub(crate) fn cell_edit_input_disabled_for_test(&self, cx: &App) -> Option<bool> {
        self.cell_editor
            .read(cx)
            .state
            .as_ref()
            .map(|state| state.input.read(cx).is_disabled())
    }

    pub(crate) fn cell_edit_was_text_for_test(&self, cx: &App) -> Option<String> {
        self.cell_editor
            .read(cx)
            .state
            .as_ref()
            .map(|state| state.was_text.clone())
    }

    pub(crate) fn set_cell_edit_input_for_test(&mut self, value: &str, cx: &mut Context<Self>) {
        let input = self
            .cell_editor
            .read(cx)
            .state
            .as_ref()
            .map(|state| state.input.clone());
        if let Some(input) = input {
            input.update(cx, |field, cx| field.set_value(value, cx));
        }
    }

    pub(crate) fn open_cell_edit_for_test(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_cell_edit(row, col, None, window, cx);
    }

    pub(crate) fn set_cell_edit_mode_for_test(
        &mut self,
        mode: CellEditMode,
        cx: &mut Context<Self>,
    ) {
        self.cell_editor
            .update(cx, |editor, cx| editor.set_mode(mode, cx));
    }

    pub(crate) fn stage_cell_edit_for_test(&mut self, cx: &mut Context<Self>) {
        self.cell_editor.update(cx, CellEditor::stage);
    }

    pub(crate) fn cancel_cell_edit_for_test(&mut self, cx: &mut Context<Self>) {
        self.cell_editor.update(cx, CellEditor::cancel);
    }

    pub(crate) fn cell_edit_eligible_for_test(&self, cx: &App, row: usize, col: usize) -> bool {
        self.cell_edit_eligible(cx, row, col)
    }
}
