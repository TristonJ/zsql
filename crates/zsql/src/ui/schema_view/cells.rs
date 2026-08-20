//! Pure cell/badge rendering helpers for the schema tab's Columns, Indexes,
//! and Constraints tables: no `self`, no window/context, just data in and
//! an element out.

use gpui::{IntoElement, div, prelude::*, px, rgb};
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, DefaultKind, KeyCellBadge, RelationKind,
    RowCount, classify_default, key_cell_badge,
};
use zsql_ui::grid;
use zsql_ui::table::TableStyle;
use zsql_ui::theme::Theme;

use super::theme;

/// Row height for the Columns, Indexes, and Constraints tables: taller than
/// the grid's default row height for schema-browsing readability.
const SCHEMA_TABLE_ROW_HEIGHT: gpui::Pixels = px(36.0);

/// The style shared by the Columns, Indexes, and Constraints tables.
pub(super) fn schema_table_style(theme: &Theme) -> TableStyle {
    TableStyle {
        row_height: SCHEMA_TABLE_ROW_HEIGHT,
        ..TableStyle::themed(theme)
    }
}

/// One header stat, e.g. `~1,240` bolded followed by a faint ` rows` label.
pub(super) fn stat_label(value: &str, label: &str, active_theme: &Theme) -> impl IntoElement {
    div()
        .child(format!("{value} "))
        .text_color(rgb(active_theme.colors.text_secondary))
        .child(
            div()
                .text_color(rgb(active_theme.colors.text_tertiary))
                .child(label.to_owned()),
        )
}

/// A section: an uppercase label with a trailing count pill, followed by
/// `table`.
pub(super) fn section(
    label: &str,
    count: usize,
    table: impl IntoElement,
    active_theme: &Theme,
) -> impl IntoElement {
    let colors = active_theme.colors;
    div()
        .flex()
        .flex_col()
        .w(theme::SCHEMA_SECTION_WIDTH)
        .max_w_full()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mb(theme::SCHEMA_SECTION_LABEL_MARGIN_BOTTOM)
                .text_size(px(theme::SCHEMA_SECTION_LABEL_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(label.to_uppercase())
                .child(
                    div()
                        .text_color(rgb(colors.text_secondary))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .rounded(px(theme::SCHEMA_SECTION_COUNT_PILL_RADIUS))
                        .px(theme::SCHEMA_SECTION_COUNT_PILL_PADDING_X)
                        .child(count.to_string()),
                ),
        )
        .child(table)
}

/// Get the default cell horizontal padding to use, if needed.
pub(super) fn cell_x_padding() -> gpui::Pixels {
    px(grid::CELL_PADDING_X)
}

pub(super) fn header_cell(text: String, active_theme: &Theme) -> gpui::Div {
    div()
        .text_color(rgb(active_theme.colors.text_primary))
        .child(text)
}

/// The Null cell's text and color for `nullable`.
pub(super) fn null_label(nullable: bool, active_theme: &Theme) -> gpui::Div {
    if nullable {
        div()
            .italic()
            .text_color(rgb(active_theme.colors.text_tertiary))
            .child(theme::SCHEMA_NULLABLE_LABEL)
    } else {
        div()
            .text_color(rgb(active_theme.colors.text_secondary))
            .child(theme::SCHEMA_NOT_NULL_LABEL)
    }
}

/// The Default cell: violet for a function call, amber for a literal, a
/// faint dash placeholder for none.
pub(super) fn render_default_cell(default: Option<&str>, active_theme: &Theme) -> gpui::Div {
    let colors = active_theme.colors;
    match classify_default(default) {
        DefaultKind::None => div()
            .text_color(rgb(colors.text_tertiary))
            .child(theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER),
        DefaultKind::Literal => div()
            .text_color(rgb(colors.value_bool))
            .child(default.unwrap_or_default().to_owned()),
        DefaultKind::Function => div()
            .text_color(rgb(colors.value_number))
            .child(default.unwrap_or_default().to_owned()),
    }
}

/// The Keys cell: a PK/unique/check badge, an FK link chip, or nothing.
pub(super) fn render_keys_cell(
    column: &ColumnDetail,
    constraints: &[ConstraintInfo],
    active_theme: &Theme,
) -> gpui::AnyElement {
    let colors = active_theme.colors;
    match key_cell_badge(column, constraints) {
        None => div().into_any_element(),
        Some(KeyCellBadge::Primary) => key_badge(
            theme::SCHEMA_BADGE_PK_LABEL,
            colors.accent,
            theme::schema_badge_pk_border(active_theme),
        )
        .into_any_element(),
        Some(KeyCellBadge::Unique) => key_badge(
            theme::SCHEMA_BADGE_UNIQUE_LABEL,
            colors.value_bool,
            colors.warn_outline(),
        )
        .into_any_element(),
        Some(KeyCellBadge::Check) => key_badge(
            theme::SCHEMA_BADGE_CHECK_LABEL,
            colors.text_secondary,
            rgb(colors.border),
        )
        .into_any_element(),
        Some(KeyCellBadge::Foreign(target)) => {
            fk_link_chip(&target, active_theme).into_any_element()
        }
    }
}

/// A small outlined badge for the Keys cell (PK, unique, or check).
pub(super) fn key_badge(label: &str, text_color: u32, border_color: gpui::Rgba) -> gpui::Div {
    div()
        .text_size(px(theme::SCHEMA_BADGE_TEXT_SIZE))
        .text_color(rgb(text_color))
        .border_1()
        .border_color(border_color)
        .rounded(px(theme::SCHEMA_BADGE_RADIUS))
        .px(theme::SCHEMA_BADGE_PADDING_X)
        .child(label.to_owned())
}

/// The `-> target.column` foreign-key link chip.
pub(super) fn fk_link_chip(target: &str, active_theme: &Theme) -> gpui::Div {
    let colors = active_theme.colors;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .text_size(px(theme::SCHEMA_FK_CHIP_TEXT_SIZE))
        .font_family(&active_theme.fonts.data)
        .text_color(rgb(colors.key_fk))
        .bg(colors.fk_wash())
        .border_1()
        .border_color(colors.fk_outline())
        .rounded(px(theme::SCHEMA_FK_CHIP_RADIUS))
        .px(theme::SCHEMA_BADGE_PADDING_X)
        .child(theme::SCHEMA_FK_ARROW)
        .child(target.to_owned())
}

/// The label and text color a [`ConstraintKind`] renders its type badge
/// with.
pub(super) fn constraint_kind_badge(
    kind: ConstraintKind,
    active_theme: &Theme,
) -> (&'static str, u32) {
    let colors = active_theme.colors;
    match kind {
        ConstraintKind::PrimaryKey => ("PRIMARY KEY", colors.accent),
        ConstraintKind::ForeignKey => ("FOREIGN KEY", colors.key_fk),
        ConstraintKind::Unique => ("UNIQUE", colors.value_bool),
        ConstraintKind::Check => ("CHECK", colors.text_secondary),
    }
}

/// The uppercase kind-pill text for a relation's kind.
pub(super) fn relation_kind_pill_text(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "TABLE",
        RelationKind::View => "VIEW",
        RelationKind::MatView => "MATERIALIZED VIEW",
        RelationKind::Partitioned => "PARTITIONED TABLE",
    }
}

/// The header stat's row-count text: grouped digits, `~`-prefixed when
/// estimated, or [`theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER`] while the count
/// has not arrived yet.
pub(super) fn format_row_count_stat(row_count: Option<RowCount>) -> String {
    row_count.map_or_else(
        || theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER.to_owned(),
        RowCount::grouped_display,
    )
}

/// The left key-rail tick color for `column`: teal for a primary key, the
/// link hue for a foreign key, or none.
pub(super) fn rail_color(column: &ColumnDetail, active_theme: &Theme) -> Option<u32> {
    if column.is_primary_key {
        Some(active_theme.colors.accent)
    } else if column.foreign_key.is_some() {
        Some(active_theme.colors.key_fk)
    } else {
        None
    }
}
