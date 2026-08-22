//! The "Run with parameters" modal: one row per detected parameter (`:name`
//! on every driver, `@name` on mssql, or a positional `?1`/`?2`/... on
//! mysql and sqlite), each showing its inferred type, the query line it
//! comes from with the real token highlighted, and a `zsql_ui` text field
//! prefilled from the last run.

mod bindings;
mod logic;

use std::collections::HashMap;

pub(crate) use bindings::ParametersModalBindings;
use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    KeybindingKeystroke, Render, Window, div, prelude::*, px, rgb, rgba,
};
use logic::RowContext;
use zsql_core::sql::params::{ParamKind, ParamType, Parameter, substitute_params};
use zsql_ui::button::{primary_button, secondary_button};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::modal::{Modal, ModalSize};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState, TextFieldStyle};
use zsql_ui::theme::{ActiveTheme, Colors};

use super::tabs::TabId;
use crate::keybindings::parse_keystrokes;
use crate::ui::theme;

/// One parameter's row: its display context plus the live field entity it
/// renders.
struct ParamField {
    row: RowContext,
    field: Entity<TextFieldState>,
}

/// The tab and query a modal session is open for.
struct OpenState {
    tab_id: TabId,
    /// The eyebrow's script name (the tab's own title).
    script_label: String,
    /// The raw SQL, every parameter token intact, that [`Self::parameters`]
    /// was detected from.
    sql: String,
    parameters: Vec<Parameter>,
    /// The remembered-values scope this run's fields save into on confirm;
    /// see `crate::session_store::ScriptBacking::param_history_key`.
    history_key: String,
    /// The active connection's driver id ([`zsql_core::driver::Driver::id`]),
    /// deciding how [`substitute_params`] escapes each value on confirm.
    driver_id: &'static str,
}

/// What the modal asks its host to do.
#[derive(Debug)]
pub enum ParametersModalEvent {
    /// The user filled in every field and confirmed (Enter or "Run query").
    Confirmed {
        tab_id: TabId,
        /// `sql` with every parameter token replaced by its field's value.
        substituted_sql: String,
        history_key: String,
        /// The raw (unsubstituted) values entered, keyed by each
        /// parameter's own storage key, for the caller to remember for
        /// next time.
        values: HashMap<String, String>,
    },
    /// The user cancelled (Escape, the close icon, or Cancel). No query was
    /// run and no remembered value changed.
    Cancelled,
}

impl EventEmitter<ParametersModalEvent> for ParametersModalView {}

/// The "Run with parameters" modal's state: whether it is open (and for
/// which tab/query), and one field per detected parameter.
pub struct ParametersModalView {
    open: Option<OpenState>,
    fields: Vec<ParamField>,
    modal_focus: FocusHandle,
    cancel_focus: FocusHandle,
    run_focus: FocusHandle,
    /// Set by [`Self::open`], consumed by the next `render`: focuses the
    /// first field so keystrokes reach it immediately instead of whatever
    /// held focus before the modal opened.
    refocus_first_field: bool,
    /// The keystroke(s) that move focus to the next control in
    /// [`Self::focus_order`]; see [`Self::configure_keybindings`].
    next_field_keystrokes: Vec<KeybindingKeystroke>,
    /// The keystroke(s) that move focus to the previous control in
    /// [`Self::focus_order`]; see [`Self::configure_keybindings`].
    previous_field_keystrokes: Vec<KeybindingKeystroke>,
}

impl ParametersModalView {
    #[must_use]
    pub fn new(cx: &mut Context<Self>) -> Self {
        let defaults = ParametersModalBindings::default();
        Self {
            open: None,
            fields: Vec::new(),
            modal_focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle(),
            run_focus: cx.focus_handle(),
            refocus_first_field: false,
            next_field_keystrokes: parse_keystrokes(
                &defaults.next_field,
                "keybindings.parameters_modal.next_field",
            ),
            previous_field_keystrokes: parse_keystrokes(
                &defaults.previous_field,
                "keybindings.parameters_modal.previous_field",
            ),
        }
    }

    /// Override the Tab/Shift-Tab field-navigation keystrokes seeded from
    /// [`ParametersModalBindings::default`], with `bindings`'s resolved
    /// configuration. Call once at construction, before any window that
    /// hosts this view is shown.
    pub fn configure_keybindings(&mut self, bindings: &ParametersModalBindings) {
        self.next_field_keystrokes = parse_keystrokes(
            &bindings.next_field,
            "keybindings.parameters_modal.next_field",
        );
        self.previous_field_keystrokes = parse_keystrokes(
            &bindings.previous_field,
            "keybindings.parameters_modal.previous_field",
        );
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// How many parameters the currently open query has, for the status
    /// bar's count while this modal is open.
    #[must_use]
    pub fn parameter_count(&self) -> usize {
        self.fields.len()
    }

    /// Open the modal for `tab_id`'s `sql`, whose `parameters` were already
    /// detected by the caller (see
    /// `zsql_core::sql::params::detect_parameters`). `history` holds each
    /// parameter's remembered values (most recent first), keyed by
    /// `zsql_core::sql::params::Parameter::storage_key`; a key with no
    /// entry seeds an empty field. `history_key` scopes where confirming
    /// this run saves new values back to. `driver_id` is the active
    /// connection's driver id, deciding how confirming this run escapes
    /// each value.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(
        name = "parameters_modal_open",
        skip(self, sql, parameters, history, cx)
    )]
    pub fn open(
        &mut self,
        tab_id: TabId,
        script_label: String,
        sql: String,
        parameters: Vec<Parameter>,
        mut history: HashMap<String, Vec<String>>,
        history_key: String,
        driver_id: &'static str,
        cx: &mut Context<Self>,
    ) {
        let rows = logic::build_row_contexts(&sql, &parameters);
        self.fields = rows
            .into_iter()
            .map(|row| {
                let initial = history
                    .remove(&row.key)
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                let field = cx.new(|cx| {
                    TextFieldState::new("Enter a value...", Some(&initial), cx).style(
                        TextFieldStyle {
                            border_w: px(0.0),
                            ..TextFieldStyle::default()
                        },
                    )
                });
                cx.subscribe(&field, |view, _field, event, cx| {
                    if matches!(event, TextFieldEvent::Submit) {
                        view.confirm(cx);
                    }
                })
                .detach();
                ParamField { row, field }
            })
            .collect();
        self.open = Some(OpenState {
            tab_id,
            script_label,
            sql,
            parameters,
            history_key,
            driver_id,
        });
        self.refocus_first_field = true;
        cx.notify();
    }

    /// Close the modal without running the query, if it was open. No
    /// remembered value is mutated.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            self.fields.clear();
            cx.emit(ParametersModalEvent::Cancelled);
            cx.notify();
        }
    }

    /// Whether every field currently holds a non-empty value.
    #[must_use]
    fn can_submit(&self, cx: &App) -> bool {
        !self.fields.is_empty()
            && self
                .fields
                .iter()
                .all(|field| !field.field.read(cx).value().is_empty())
    }

    /// Confirm the current field values: rejects (leaving the modal open,
    /// each empty field still showing its error-tinted state) while any
    /// field is empty, otherwise substitutes every value into the original
    /// SQL and emits [`ParametersModalEvent::Confirmed`].
    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            cx.notify();
            return;
        }
        let Some(open) = self.open.take() else {
            return;
        };
        let values: HashMap<String, String> = self
            .fields
            .iter()
            .map(|field| {
                (
                    field.row.key.clone(),
                    field.field.read(cx).value().to_string(),
                )
            })
            .collect();
        let substituted_sql =
            substitute_params(&open.sql, &open.parameters, &values, open.driver_id);
        self.fields.clear();
        cx.emit(ParametersModalEvent::Confirmed {
            tab_id: open.tab_id,
            substituted_sql,
            history_key: open.history_key,
            values,
        });
        cx.notify();
    }

    /// Every focusable control in visual order: each field, then Cancel,
    /// then Run query.
    fn focus_order(&self, cx: &App) -> Vec<FocusHandle> {
        let mut order: Vec<FocusHandle> = self
            .fields
            .iter()
            .map(|field| field.field.read(cx).focus_handle(cx))
            .collect();
        order.push(self.cancel_focus.clone());
        order.push(self.run_focus.clone());
        order
    }

    /// Move focus to the next (or, if `backward`, previous) control in
    /// [`Self::focus_order`], wrapping past either end.
    fn move_focus(&self, backward: bool, window: &mut Window, cx: &Context<Self>) {
        let order = self.focus_order(cx);
        if order.is_empty() {
            return;
        }
        let current = window.focused(cx);
        let current_index = current.and_then(|handle| order.iter().position(|f| *f == handle));
        let next_index = match current_index {
            Some(index) if backward => (index + order.len() - 1) % order.len(),
            Some(index) => (index + 1) % order.len(),
            None => 0,
        };
        window.focus(&order[next_index]);
    }

    /// `Escape` closes the modal (also independently handled by the shared
    /// `Modal` component's own scrim). `Tab`/`Shift-Tab` (or whatever
    /// [`Self::configure_keybindings`] resolved them to) move focus through
    /// [`Self::focus_order`]. Every other key is left to a field's own
    /// bindings (character entry, Enter-to-submit).
    fn handle_modal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key.as_str() == "escape" {
            self.close(cx);
        } else if self
            .next_field_keystrokes
            .iter()
            .any(|k| event.keystroke.should_match(k))
        {
            self.move_focus(false, window, cx);
        } else if self
            .previous_field_keystrokes
            .iter()
            .any(|k| event.keystroke.should_match(k))
        {
            self.move_focus(true, window, cx);
        }
    }

    fn render_row(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let colors = cx.theme().colors;
        let field = &self.fields[index];
        let row = field.row.clone();
        let is_empty = field.field.read(cx).value().is_empty();
        let focused = field.field.read(cx).focus_handle(cx).is_focused(window);
        let badge_color = type_badge_color(row.param_type, colors);

        div()
            .id(("parameters-modal-row", index))
            .flex()
            .flex_col()
            .gap(px(9.0))
            .py(px(15.0))
            .when(index > 0, |el| {
                el.border_t_1().border_color(rgb(colors.border_soft))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .font_family(&cx.theme().fonts.data)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(px(13.0))
                            .text_color(rgb(colors.accent))
                            .child(row_label(&row)),
                    )
                    .child(
                        div()
                            .ml(px(8.0))
                            .font_family(&cx.theme().fonts.data)
                            .text_size(px(9.5))
                            .text_color(rgb(badge_color))
                            .border_1()
                            .border_color(rgba((badge_color << 8) | 0x33))
                            .rounded(px(4.0))
                            .px(px(6.0))
                            .py(px(1.0))
                            .child(row.param_type.label()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(14.0))
                    .items_start()
                    .child(render_snippet(&row, cx))
                    .child(self.render_value_column(index, is_empty, focused, cx)),
            )
    }

    fn render_value_column(
        &self,
        index: usize,
        is_empty: bool,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let colors = cx.theme().colors;
        let field = &self.fields[index];
        let border_color = if is_empty {
            rgba((colors.status_error << 8) | 0x52)
        } else if focused {
            rgb(colors.accent)
        } else {
            rgb(colors.border)
        };

        div()
            .flex_shrink_0()
            .w(px(240.0))
            .h(px(38.0))
            .bg(rgb(colors.bg_app))
            .border_1()
            .border_color(border_color)
            .rounded(px(7.0))
            .px(px(11.0))
            .flex()
            .items_center()
            .child(field.field.clone())
    }

    fn render_footer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let can_submit = self.can_submit(cx);
        let hints = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(14.0))
            .text_size(px(12.0))
            .text_color(rgb(colors.text_secondary))
            .child(hint_chip("Tab", "next", cx))
            .child(hint_chip("Enter", "run", cx))
            .child(hint_chip("Esc", "cancel", cx));

        zsql_ui::modal::footer_bar(cx)
            .child(hints)
            .child(div().flex_1())
            .child(
                secondary_button("parameters-modal-cancel", window, cx)
                    .track_focus(&self.cancel_focus)
                    .child("Cancel")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.close(cx);
                    })),
            )
            .child({
                let hover_bg = theme::run_button_hover_bg(cx.theme());
                let button = primary_button("parameters-modal-run", window, cx)
                    .track_focus(&self.run_focus)
                    // Solid like the toolbar Run button, not the outline
                    // primary style.
                    .bg(rgb(colors.accent))
                    .border_color(rgb(colors.accent))
                    .text_color(rgb(colors.bg_app))
                    .font_weight(gpui::FontWeight::BOLD)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(icon(
                        IconName::Run,
                        theme::RUN_BUTTON_ICON_SIZE,
                        colors.bg_app,
                    ))
                    .child("Run query");
                if can_submit {
                    button
                        .hover(move |el| el.bg(rgb(hover_bg)))
                        .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                            view.confirm(cx);
                        }))
                } else {
                    button.opacity(theme::CONNECTION_FORM_DIM_OPACITY)
                }
            })
    }
}

/// The row's own display label: `:name` for a colon parameter, `@name` for
/// a T-SQL one, or the bare `?1`/`?2`/... a positional row's name already
/// is.
fn row_label(row: &RowContext) -> String {
    match row.kind {
        ParamKind::Colon => format!(":{}", row.name),
        ParamKind::At => format!("@{}", row.name),
        ParamKind::Positional => row.name.clone(),
    }
}

/// The badge color a parameter's inferred type renders with.
fn type_badge_color(param_type: ParamType, colors: Colors) -> u32 {
    match param_type {
        ParamType::Date => colors.syntax_string,
        ParamType::Numeric | ParamType::Integer => colors.value_number,
        ParamType::Text => colors.text_tertiary,
    }
}

fn render_snippet(row: &RowContext, cx: &Context<ParametersModalView>) -> Div {
    let colors = cx.theme().colors;
    let before = &row.line_text[..row.token_start];
    let token = &row.line_text[row.token_start..row.token_end];
    let after = &row.line_text[row.token_end..];
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_row()
        .h(px(38.0))
        .bg(rgb(colors.bg_app))
        .border_1()
        .border_color(rgb(colors.border_soft))
        .rounded(px(8.0))
        .overflow_hidden()
        .font_family(&cx.theme().fonts.data)
        .text_size(px(12.0))
        .text_color(rgb(colors.text_secondary))
        .child(
            div()
                .flex_shrink_0()
                .w(px(30.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba((colors.border_soft << 8) | 0x99))
                .border_r_1()
                .border_color(rgb(colors.border_soft))
                .text_size(px(10.5))
                .text_color(rgb(colors.text_tertiary))
                .child(row.line.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .px(px(12.0))
                .child(before.to_owned())
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.accent))
                        .bg(rgba((colors.accent << 8) | 0x22))
                        .border_1()
                        .border_color(rgba((colors.accent << 8) | 0x55))
                        .rounded(px(4.0))
                        .px(px(4.0))
                        .child(token.to_owned()),
                )
                .child(after.to_owned()),
        )
}

fn hint_chip(key: &'static str, label: &'static str, cx: &Context<ParametersModalView>) -> Div {
    let colors = cx.theme().colors;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .font_family(&cx.theme().fonts.data)
                .text_size(px(10.5))
                .text_color(rgb(colors.text_primary))
                .bg(rgb(colors.bg_app))
                .border_1()
                .border_color(rgb(colors.border))
                .rounded(px(5.0))
                .px(px(6.0))
                .child(key),
        )
        .child(label)
}

impl Render for ParametersModalView {
    /// The caller is responsible for conditionally mounting this entity
    /// (only while [`Self::is_open`]), so `render` does not re-check that.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.refocus_first_field) {
            let order = self.focus_order(cx);
            if let Some(first) = order.first() {
                window.focus(first);
            }
        }
        let colors = cx.theme().colors;
        let script_label = self
            .open
            .as_ref()
            .map(|open| open.script_label.clone())
            .unwrap_or_default();
        let count = self.fields.len();
        let subtitle = format!(
            "{count} parameter{} found. Prefilled with each parameter's value from the last run.",
            if count == 1 { "" } else { "s" }
        );

        // The shared head container pads 12px; another 12px here lines the
        // head text up with the body rows' 24px inset.
        let head = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(7.0))
                    .font_family(&cx.theme().fonts.data)
                    .text_size(px(10.0))
                    .text_color(rgb(colors.accent))
                    .child("PARAMETERS")
                    .child(
                        div()
                            .text_color(rgb(colors.text_tertiary))
                            .child(format!("\u{b7} {script_label}")),
                    ),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(px(17.0))
                    .text_color(rgb(colors.text_primary))
                    .child("Run with parameters"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(colors.text_secondary))
                    .child(subtitle),
            );

        let mut body = div()
            .id("parameters-modal-body")
            .flex()
            .flex_col()
            .max_h(px(420.0))
            .overflow_y_scroll()
            .px(px(24.0));
        for index in 0..self.fields.len() {
            body = body.child(self.render_row(index, window, cx));
        }

        let footer = self.render_footer(window, cx);

        Modal::<Div, Div>::new("parameters-modal")
            .size(ModalSize::Large)
            .width(px(720.0))
            .track_focus(&self.modal_focus)
            .on_close(cx.listener(|view, (), _w, cx| view.close(cx)))
            .head(head)
            .body(
                div()
                    .flex()
                    .flex_col()
                    .on_key_down(cx.listener(Self::handle_modal_key_down))
                    .child(body)
                    .child(footer),
            )
    }
}

/// Test-only accessor: production rendering reads a field's value straight
/// off its own `TextFieldState` entity.
#[cfg(test)]
impl ParametersModalView {
    pub(crate) fn field_value_for_test(&self, index: usize, cx: &App) -> String {
        self.fields[index].field.read(cx).value().to_string()
    }
}

#[cfg(test)]
mod tests;
