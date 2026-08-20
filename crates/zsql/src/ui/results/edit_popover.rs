//! The cell edit popover: a small overlay anchored to the edited cell,
//! opened on an edit-eligible cell's double-click or F2.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Corner, Div, Entity, MouseButton, Pixels, Point, RenderOnce, Stateful,
    TextOverflow, Window, anchored, deferred, div, prelude::*, px, rgb,
};
use zsql_ui::text_field::TextFieldState;
use zsql_ui::theme::{ActiveTheme, Colors};

use zsql_core::sql::quote_ident;
use zsql_editor::{HighlightKind, syntax_color};

use super::CancelCellEdit;
use super::cell_edit::CellEditMode;
use crate::staging::UpdateValue;
use crate::ui::theme as app_theme;

/// One run of the popover's rendered `SET` fragment: its text and the
/// editor highlight kind that colors it.
pub(super) type SqlRun = (String, HighlightKind);

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type CancelHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// The cell edit popover component: pure presentation over state owned by
/// [`super::cell_edit::CellEditState`].
#[derive(IntoElement)]
pub(super) struct CellEditPopover {
    column: String,
    type_name: String,
    was_text: String,
    input: Entity<TextFieldState>,
    mode: CellEditMode,
    rendered: Vec<SqlRun>,
    /// Where the popover's top-left corner sits in window coordinates.
    /// `None` falls back to a fixed position over the grid (e.g. the cell's
    /// bounds were not yet known when the popover opened).
    anchor: Option<Point<Pixels>>,
    on_pick_literal: Option<ClickHandler>,
    on_pick_expression: Option<ClickHandler>,
    on_pick_null: Option<ClickHandler>,
    on_stage: Option<ClickHandler>,
    on_cancel: Option<CancelHandler>,
}

impl CellEditPopover {
    pub(super) fn new(
        column: String,
        type_name: String,
        was_text: String,
        input: Entity<TextFieldState>,
        mode: CellEditMode,
        rendered: Vec<SqlRun>,
        anchor: Option<Point<Pixels>>,
    ) -> Self {
        Self {
            column,
            type_name,
            was_text,
            input,
            mode,
            rendered,
            anchor,
            on_pick_literal: None,
            on_pick_expression: None,
            on_pick_null: None,
            on_stage: None,
            on_cancel: None,
        }
    }

    #[must_use]
    pub(super) fn on_pick_literal(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick_literal = Some(Box::new(f));
        self
    }

    #[must_use]
    pub(super) fn on_pick_expression(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick_expression = Some(Box::new(f));
        self
    }

    #[must_use]
    pub(super) fn on_pick_null(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick_null = Some(Box::new(f));
        self
    }

    /// The footer's Stage button.
    #[must_use]
    pub(super) fn on_stage(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_stage = Some(Box::new(f));
        self
    }

    /// Escape, or a click anywhere outside the popover.
    #[must_use]
    pub(super) fn on_cancel(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(f));
        self
    }
}

fn render_head(
    column: String,
    type_name: &str,
    was_text: &str,
    colors: Colors,
    theme: &zsql_ui::theme::Theme,
) -> Div {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(app_theme::EDIT_POPOVER_HEAD_GAP)
        .pt(app_theme::EDIT_POPOVER_HEAD_PADDING_TOP)
        .px(app_theme::EDIT_POPOVER_HEAD_PADDING_X)
        .text_size(px(app_theme::EDIT_POPOVER_HEAD_TEXT_SIZE))
        .text_color(rgb(colors.text_primary))
        .child(column)
        .child(zsql_ui::grid::type_tag_tertiary(type_name, theme))
        .child(
            div()
                .ml_auto()
                .text_size(px(app_theme::EDIT_POPOVER_WAS_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .text_overflow(TextOverflow::Truncate("…".into()))
                .overflow_hidden()
                .child(format!("was {was_text}")),
        )
}

/// The exact `SET "col" = value` fragment [`crate::staging::update_set_fragment_sql`]
/// embeds in the staged statement, split into colorable runs typed with the
/// editor's own [`HighlightKind`]s.
pub(super) fn set_fragment_runs(column: &str, new_value: &UpdateValue) -> Vec<SqlRun> {
    let value_run = match new_value {
        UpdateValue::Null => ("NULL".to_owned(), HighlightKind::Keyword),
        UpdateValue::Expression(text) => (text.clone(), HighlightKind::Number),
        UpdateValue::Literal(text) => {
            let kind = if text.starts_with('\'') {
                HighlightKind::String
            } else {
                HighlightKind::Number
            };
            (text.clone(), kind)
        }
    };
    vec![
        ("SET ".to_owned(), HighlightKind::Keyword),
        (quote_ident(column), HighlightKind::String),
        (" = ".to_owned(), HighlightKind::Operator),
        value_run,
    ]
}

fn render_preview(rendered: &[SqlRun], theme: &zsql_ui::theme::Theme) -> Div {
    let colors = theme.colors;
    let mut line = div()
        .flex()
        .flex_row()
        .mx(app_theme::EDIT_POPOVER_HEAD_PADDING_X)
        .mt(app_theme::EDIT_POPOVER_RENDER_MARGIN_TOP)
        .px(app_theme::EDIT_POPOVER_RENDER_PADDING_X)
        .py(app_theme::EDIT_POPOVER_RENDER_PADDING_Y)
        .rounded(px(app_theme::EDIT_POPOVER_RENDER_RADIUS))
        .border_1()
        .border_color(rgb(colors.border_soft))
        .bg(rgb(colors.bg_app))
        .overflow_hidden()
        .text_size(px(app_theme::EDIT_POPOVER_RENDER_TEXT_SIZE));
    for (text, kind) in rendered {
        line = line.child(
            div()
                .text_color(rgb(syntax_color(theme, *kind)))
                .child(text.clone()),
        );
    }
    line
}

fn render_chip(
    id: &'static str,
    label: &str,
    on: bool,
    mauve_when_on: bool,
    colors: Colors,
    on_click: Option<ClickHandler>,
) -> Stateful<Div> {
    let accent = if mauve_when_on {
        colors.value_number
    } else {
        colors.status_warn
    };
    let mut chip = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .cursor_pointer()
        .flex()
        .flex_row()
        .items_center()
        .gap(app_theme::EDIT_POPOVER_CHIP_INNER_GAP)
        .h(app_theme::EDIT_POPOVER_CHIP_HEIGHT)
        .px(app_theme::EDIT_POPOVER_CHIP_PADDING_X)
        .rounded(px(app_theme::EDIT_POPOVER_CHIP_RADIUS))
        .border_1()
        .text_size(px(app_theme::EDIT_POPOVER_CHIP_TEXT_SIZE))
        .child(label.to_owned());
    chip = if on {
        chip.text_color(rgb(accent))
            .border_color(Colors::wash(accent, 0x66))
            .bg(Colors::wash(accent, 0x14))
    } else {
        chip.text_color(rgb(colors.text_tertiary))
            .border_color(rgb(colors.border))
            .hover(|el| el.text_color(rgb(colors.text_primary)))
    };
    if let Some(on_click) = on_click {
        chip = chip.on_click(on_click);
    }
    chip
}

/// The three mode chips, mutually exclusive.
fn render_modes(mode: CellEditMode, colors: Colors, popover: &mut CellEditPopover) -> Div {
    let literal = render_chip(
        "edit-popover-mode-literal",
        "'abc' literal",
        mode == CellEditMode::Literal,
        false,
        colors,
        popover.on_pick_literal.take(),
    );
    let expression = render_chip(
        "edit-popover-mode-expression",
        "fx expression",
        mode == CellEditMode::Expression,
        true,
        colors,
        popover.on_pick_expression.take(),
    );
    let null = render_chip(
        "edit-popover-mode-null",
        "set NULL",
        mode == CellEditMode::Null,
        false,
        colors,
        popover.on_pick_null.take(),
    );

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_start()
        .gap(app_theme::EDIT_POPOVER_CHIP_GAP)
        .mx(app_theme::EDIT_POPOVER_HEAD_PADDING_X)
        .mt(app_theme::EDIT_POPOVER_MODES_MARGIN_TOP)
        .child(literal)
        .child(expression)
        .child(div().child(null))
}

/// The footer's keyboard hints: "Enter stage", "Esc cancel".
/// The footer's Stage button, mirroring the staging bar's own amber Apply.
fn render_stage_button(colors: Colors, on_stage: Option<ClickHandler>) -> Stateful<Div> {
    let mut button = div()
        .id("edit-popover-stage")
        .debug_selector(|| "edit-popover-stage".to_owned())
        .cursor_pointer()
        .flex()
        .items_center()
        .h(app_theme::EDIT_POPOVER_CHIP_HEIGHT)
        .px(app_theme::EDIT_POPOVER_CHIP_PADDING_X)
        .rounded(px(app_theme::EDIT_POPOVER_CHIP_RADIUS))
        .border_1()
        .border_color(colors.warn_outline())
        .bg(Colors::wash(colors.status_warn, 0x24))
        .text_size(px(app_theme::EDIT_POPOVER_CHIP_TEXT_SIZE))
        .text_color(rgb(colors.text_primary))
        .child("Stage");
    if let Some(on_stage) = on_stage {
        button = button.on_click(on_stage);
    }
    button
}

fn render_foot(colors: Colors, on_stage: Option<ClickHandler>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .px(app_theme::EDIT_POPOVER_HEAD_PADDING_X)
        .pt(app_theme::EDIT_POPOVER_FOOT_PADDING_TOP)
        .pb(app_theme::EDIT_POPOVER_FOOT_PADDING_BOTTOM)
        .text_size(px(app_theme::EDIT_POPOVER_FOOT_TEXT_SIZE))
        .text_color(rgb(colors.text_tertiary))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(app_theme::EDIT_POPOVER_FOOT_GAP)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(app_theme::EDIT_POPOVER_HINT_GAP)
                        .child(div().text_color(rgb(colors.text_secondary)).child("Enter"))
                        .child("stage"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(app_theme::EDIT_POPOVER_HINT_GAP)
                        .child(div().text_color(rgb(colors.text_secondary)).child("Esc"))
                        .child("cancel"),
                ),
        )
        .child(div().ml_auto().child(render_stage_button(colors, on_stage)))
}

impl RenderOnce for CellEditPopover {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let mode = self.mode;
        let rendered = std::mem::take(&mut self.rendered);
        let input = self.input.clone();
        let anchor = self.anchor;

        let on_cancel = self.on_cancel.take();
        let on_stage = self.on_stage.take();
        let panel = div()
            .id("edit-popover")
            .debug_selector(|| "edit-popover".to_owned())
            .key_context(super::cell_edit::KEY_CONTEXT)
            .occlude()
            .relative()
            .flex()
            .flex_col()
            .w(app_theme::EDIT_POPOVER_WIDTH)
            .rounded(px(app_theme::EDIT_POPOVER_RADIUS))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.bg_raised))
            .shadow_lg()
            .font_family(&theme.fonts.data)
            .when_some(on_cancel.clone(), |el, on_cancel| {
                el.on_action(move |_: &CancelCellEdit, window, cx| on_cancel(window, cx))
            })
            .child(render_head(
                self.column.clone(),
                &self.type_name,
                &self.was_text,
                colors,
                &theme,
            ))
            .child(
                div()
                    .mt(app_theme::EDIT_POPOVER_INPUT_MARGIN_TOP)
                    .mx(app_theme::EDIT_POPOVER_HEAD_PADDING_X)
                    .child(input),
            )
            .child(render_preview(&rendered, &theme))
            .child(render_modes(mode, colors, &mut self))
            .child(render_foot(colors, on_stage));

        let position = anchor.unwrap_or_else(|| {
            let viewport_width = window.viewport_size().width;
            Point::new(
                viewport_width
                    - app_theme::EDIT_POPOVER_WIDTH
                    - app_theme::EDIT_POPOVER_FALLBACK_RIGHT_OFFSET,
                app_theme::EDIT_POPOVER_FALLBACK_TOP_OFFSET,
            )
        });

        let viewport_size = window.viewport_size();
        let cancel_left = on_cancel.clone();
        let backdrop = div()
            .id("edit-popover-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .w(viewport_size.width)
            .h(viewport_size.height)
            .when_some(cancel_left, |el, on_cancel| {
                el.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    on_cancel(window, cx);
                })
            })
            .when_some(on_cancel, |el, on_cancel| {
                el.on_mouse_down(MouseButton::Right, move |_, window, cx| {
                    on_cancel(window, cx);
                })
            });

        deferred(
            div()
                .absolute()
                .child(anchored().position(Point::default()).child(backdrop))
                .child(
                    anchored()
                        .position(position)
                        .anchor(Corner::TopLeft)
                        .snap_to_window()
                        .child(panel),
                ),
        )
        .with_priority(1)
    }
}

#[cfg(test)]
mod tests {
    use crate::staging::{UpdateValue, update_set_fragment_sql};

    use super::set_fragment_runs;

    /// The popover's preview runs concatenate to exactly the fragment the
    /// staged statement embeds, so the preview and the executed SQL can
    /// never diverge.
    #[test]
    fn set_fragment_runs_concatenate_to_the_staged_fragment() {
        for value in [
            UpdateValue::Literal("'shipped'".to_owned()),
            UpdateValue::Literal("9000".to_owned()),
            UpdateValue::Expression("now()".to_owned()),
            UpdateValue::Null,
        ] {
            let joined: String = set_fragment_runs("status", &value)
                .into_iter()
                .map(|(text, _kind)| text)
                .collect();
            assert_eq!(joined, update_set_fragment_sql("status", &value));
        }
    }
}
