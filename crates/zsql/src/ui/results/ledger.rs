//! The staged-changes ledger: "review sql" expands this panel, listing the
//! exact statement Apply will run for every staged change, syntax-
//! highlighted, each with its source row and a per-line unstage control.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, ClickEvent, Div, HighlightStyle, RenderOnce, StyledText, Window, div, prelude::*, px, rgb,
};
use zsql_editor::{StyleSpan, syntax_color};
use zsql_ui::theme::{ActiveTheme, Colors, Theme};

use crate::staging::StagedChangeId;
use crate::ui::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LedgerLineKind {
    Delete,
    Update,
}

/// One ledger row's data: the statement it will run (with its highlight
/// spans), where it was staged from, and the database's own error text if it
/// was the statement that failed the most recent Apply.
pub(super) struct LedgerLine {
    pub id: StagedChangeId,
    pub kind: LedgerLineKind,
    pub source_row: usize,
    pub sql: String,
    pub spans: Vec<StyleSpan>,
    pub error: Option<String>,
}

type UnstageHandler = Rc<dyn Fn(&StagedChangeId, &mut Window, &mut App) + 'static>;

/// The expanded "review sql" panel, docked above the staging bar: one line
/// per staged change, in the same FIFO order Apply executes them in.
#[derive(IntoElement)]
pub(super) struct StagedLedger {
    lines: Vec<LedgerLine>,
    on_unstage: Option<UnstageHandler>,
}

impl StagedLedger {
    pub(super) fn new(lines: Vec<LedgerLine>) -> Self {
        Self {
            lines,
            on_unstage: None,
        }
    }

    #[must_use]
    pub(super) fn on_unstage(
        mut self,
        f: impl Fn(&StagedChangeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_unstage = Some(Rc::new(f));
        self
    }
}

/// `spans` (char-indexed within `sql`) as the byte-ranged color highlights
/// [`StyledText::with_highlights`] takes.
fn statement_highlights(
    sql: &str,
    spans: &[StyleSpan],
    theme: &Theme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut byte_offsets: Vec<usize> = sql.char_indices().map(|(offset, _)| offset).collect();
    byte_offsets.push(sql.len());
    spans
        .iter()
        .filter_map(|span| {
            let start = *byte_offsets.get(span.start)?;
            let end = *byte_offsets.get(span.end)?;
            let style = HighlightStyle {
                color: Some(rgb(syntax_color(theme, span.kind)).into()),
                ..HighlightStyle::default()
            };
            Some((start..end, style))
        })
        .collect()
}

fn render_statement(sql: String, spans: &[StyleSpan], theme: &Theme) -> Div {
    let highlights = statement_highlights(&sql, spans, theme);
    div()
        .flex_1()
        .overflow_hidden()
        .text_color(rgb(theme.colors.text_primary))
        .child(StyledText::new(sql).with_highlights(highlights))
}

impl LedgerLineKind {
    /// A ledger line's gutter mark glyph and color.
    fn mark(self, colors: Colors) -> (&'static str, u32) {
        match self {
            LedgerLineKind::Delete => ("-", colors.status_error),
            LedgerLineKind::Update => ("\u{b1}", colors.status_warn),
        }
    }
}

fn render_line(
    line: LedgerLine,
    theme: &Theme,
    data_font: String,
    on_unstage: Option<UnstageHandler>,
) -> Div {
    let id = line.id;
    let colors = theme.colors;
    let (mark, mark_color) = line.kind.mark(colors);

    let row = div()
        .debug_selector(move || format!("ledger-line-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .h(theme::LEDGER_ROW_HEIGHT)
        .font_family(data_font)
        .text_size(px(theme::LEDGER_TEXT_SIZE))
        .child(
            div()
                .flex_shrink_0()
                .w(theme::LEDGER_GUTTER_WIDTH)
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(mark_color))
                .child(mark),
        )
        .child(render_statement(line.sql, &line.spans, theme).px_3())
        .child(
            div()
                .flex_shrink_0()
                .pr_2()
                .text_size(px(theme::LEDGER_META_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child(format!("row {}", line.source_row + 1)),
        )
        .child({
            let mut x = div()
                .id(("ledger-unstage", id))
                .debug_selector(move || format!("ledger-unstage-{id}"))
                .flex_shrink_0()
                .w(theme::LEDGER_UNSTAGE_WIDTH)
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(colors.text_tertiary))
                .cursor_pointer()
                .hover(|el| el.text_color(rgb(colors.text_primary)))
                .child("\u{2715}");
            if let Some(on_unstage) = on_unstage {
                x = x.on_click(move |_event: &ClickEvent, window, cx| {
                    on_unstage(&id, window, cx);
                });
            }
            x
        });

    match line.error {
        Some(message) => div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .child(row)
            .child(
                div()
                    .debug_selector(move || format!("ledger-error-{id}"))
                    .px_3()
                    .pb_1()
                    .text_size(px(theme::LEDGER_META_TEXT_SIZE))
                    .text_color(rgb(colors.status_error))
                    .child(message),
            ),
        None => row.border_b_1().border_color(rgb(colors.border_soft)),
    }
}

impl RenderOnce for StagedLedger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let colors = theme.colors;
        let data_font = theme.fonts.data.clone();
        let on_unstage = self.on_unstage;

        div()
            .id("staged-ledger")
            .debug_selector(|| "staged-ledger".to_owned())
            .flex()
            .flex_col()
            .flex_shrink_0()
            .max_h(theme::LEDGER_MAX_HEIGHT)
            .overflow_y_scroll()
            .bg(rgb(colors.bg_app))
            .border_t_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(theme::LEDGER_HEAD_HEIGHT)
                    .px_3()
                    .text_size(px(theme::LEDGER_META_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .border_b_1()
                    .border_color(rgb(colors.border_soft))
                    .child("staged changes \u{b7} runs as one transaction \u{b7} newest last"),
            )
            .children(
                self.lines
                    .into_iter()
                    .map(|line| render_line(line, &theme, data_font.clone(), on_unstage.clone())),
            )
    }
}

#[cfg(test)]
mod tests {
    use zsql_editor::{HighlightKind, Highlighter as _, SqlHighlighter, syntax_color};
    use zsql_ui::theme::Theme;

    use super::statement_highlights;

    /// The ledger's statements are highlighted by the same tree-sitter
    /// highlighter the SQL editor uses, byte-ranged for `StyledText`.
    #[test]
    fn a_delete_statement_highlights_keywords_and_literals() {
        let sql = "DELETE FROM \"public\".\"orders\" WHERE \"id\" = 3;";
        let mut highlighter = SqlHighlighter::new();
        highlighter.set_text(sql);
        let spans = highlighter.spans_for_line(0).to_vec();
        let theme = Theme::default();

        let highlights = statement_highlights(sql, &spans, &theme);
        assert!(!highlights.is_empty());

        let keyword_color: gpui::Hsla =
            gpui::rgb(syntax_color(&theme, HighlightKind::Keyword)).into();
        let delete = highlights
            .iter()
            .find(|(range, _)| &sql[range.clone()] == "DELETE")
            .expect("DELETE must carry a highlight");
        assert_eq!(delete.1.color, Some(keyword_color));

        let number_color: gpui::Hsla =
            gpui::rgb(syntax_color(&theme, HighlightKind::Number)).into();
        let literal = highlights
            .iter()
            .find(|(range, _)| &sql[range.clone()] == "3")
            .expect("the numeric literal must carry a highlight");
        assert_eq!(literal.1.color, Some(number_color));
    }

    /// Byte ranges stay on char boundaries for multi-byte identifiers.
    #[test]
    fn highlights_land_on_char_boundaries_in_multi_byte_sql() {
        let sql = "DELETE FROM \"caf\u{e9}\".\"orders\" WHERE \"id\" = 'na\u{efdc}ve';";
        let mut highlighter = SqlHighlighter::new();
        highlighter.set_text(sql);
        let spans = highlighter.spans_for_line(0).to_vec();

        let highlights = statement_highlights(sql, &spans, &Theme::default());
        for (range, _) in &highlights {
            assert!(sql.is_char_boundary(range.start));
            assert!(sql.is_char_boundary(range.end));
        }
    }
}
