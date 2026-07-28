//! The read-only schema tab: fetches a relation's full structural detail
//! (columns, indexes, constraints) on construction and renders it once
//! ready, reusing `zsql_ui::grid` primitives and the results grid's
//! type-tag treatment.

use std::ops::Range;

use gpui::{Context, Entity, FontWeight, Render, Window, div, prelude::*, px, rgb, rgba};
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, DefaultKind, KeyCellBadge, RelationKind,
    RelationSchema, RowCount, classify_default, key_cell_badge,
};
use zsql_ui::grid;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{ScrollView, ScrollbarStyle, vertical_scroll};
use zsql_ui::table::{Table, TableColumn, TableRow, TableSizing, TableState, TableStyle};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::theme;
use crate::session::Session;

/// Row height for the Columns, Indexes, and Constraints tables: taller than
/// the grid's default row height for schema-browsing readability.
const SCHEMA_TABLE_ROW_HEIGHT: gpui::Pixels = px(36.0);

/// The style shared by the Columns, Indexes, and Constraints tables.
fn schema_table_style(theme: &Theme) -> TableStyle {
    TableStyle {
        row_height: SCHEMA_TABLE_ROW_HEIGHT,
        ..TableStyle::themed(theme)
    }
}

/// What a schema tab currently has to render.
enum FetchState {
    /// The describe fetch has not resolved yet.
    Loading,
    /// The describe fetch succeeded.
    Ready(RelationSchema),
    /// The describe fetch failed. The message is safe to show directly.
    Error(String),
}

/// The read-only schema tab view for `schema.relation`: never editable, has
/// no dirty state, and fetches its own data independently of `Session`'s
/// shared query-lifecycle state.
pub struct SchemaTabView {
    schema: String,
    relation: String,
    kind: RelationKind,
    state: FetchState,
    row_count: Option<RowCount>,
    /// State for the "Columns" table
    columns_table: Entity<TableState>,
    /// State for the "Indexes" table
    indexes_table: Entity<TableState>,
    /// State for the "Constraints" table
    constraints_table: Entity<TableState>,
    /// Vertical scroll for the whole tab body: the three `Fit` tables never
    /// scroll themselves, so the page scrolls all of them together.
    page_scroll: ScrollView,
}

impl SchemaTabView {
    /// Build a schema tab for `schema.relation` and dispatch its describe
    /// (and row-count) fetches as their own background tasks on `session`'s
    /// connection.
    #[must_use]
    pub fn new(
        session: &Entity<Session>,
        schema: String,
        relation: String,
        kind: RelationKind,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::spawn_describe(session, &schema, &relation, cx);
        Self::spawn_row_count(session, &schema, &relation, cx);
        Self {
            schema,
            relation,
            kind,
            state: FetchState::Loading,
            row_count: None,
            columns_table: cx.new(TableState::new),
            indexes_table: cx.new(TableState::new),
            constraints_table: cx.new(TableState::new),
            page_scroll: ScrollView::new(cx),
        }
    }

    /// Get the current schema, if available
    fn schema(&self) -> Option<&RelationSchema> {
        match &self.state {
            FetchState::Ready(detail) => Some(detail),
            _ => None,
        }
    }

    fn spawn_describe(
        session: &Entity<Session>,
        schema: &str,
        relation: &str,
        cx: &mut Context<Self>,
    ) {
        let task = session.update(cx, |session, cx| {
            session.describe_relation(schema, relation, cx)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.state = match result {
                    Ok(detail) => FetchState::Ready(detail),
                    Err(err) => {
                        tracing::warn!(error = %err, "schema tab describe_relation failed");
                        FetchState::Error(err.to_string())
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn spawn_row_count(
        session: &Entity<Session>,
        schema: &str,
        relation: &str,
        cx: &mut Context<Self>,
    ) {
        let task = session.update(cx, |session, cx| {
            session.relation_row_count(schema, relation, cx)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if let Ok(row_count) = result {
                    view.row_count = Some(row_count);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A centered title/detail message, used for the loading and error
    /// states.
    fn render_placeholder(
        color: u32,
        title: &str,
        detail: &str,
        active_theme: &Theme,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_size(px(theme::SCHEMA_TITLE_TEXT_SIZE))
                    .text_color(rgb(color))
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_size(px(theme::SCHEMA_STATS_TEXT_SIZE))
                    .text_color(rgb(active_theme.colors.text_tertiary))
                    .child(detail.to_owned()),
            )
    }

    /// The header meta strip: structure icon, qualified name, kind pill, and
    /// the four header counts.
    fn render_head(&self, detail: &RelationSchema, active_theme: &Theme) -> impl IntoElement {
        let colors = active_theme.colors;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .flex_shrink_0()
            .px(theme::SCHEMA_HEAD_PADDING_X)
            .pt(theme::SCHEMA_HEAD_PADDING_TOP)
            .pb(theme::SCHEMA_HEAD_PADDING_BOTTOM)
            .border_b_1()
            .border_color(rgb(colors.border))
            .font_family(&active_theme.fonts.data)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(icon(
                        IconName::Table,
                        px(theme::SCHEMA_TITLE_TEXT_SIZE),
                        colors.accent,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::SCHEMA_TITLE_TEXT_SIZE))
                            .text_color(rgb(colors.text_tertiary))
                            .child(format!("{}.", self.schema)),
                    )
                    .child(
                        div()
                            .text_size(px(theme::SCHEMA_TITLE_TEXT_SIZE))
                            .text_color(rgb(colors.text_primary))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.relation.clone()),
                    ),
            )
            .child(
                div()
                    .ml_2()
                    .text_size(px(theme::SCHEMA_KIND_PILL_TEXT_SIZE))
                    .text_color(rgb(colors.accent))
                    .border_1()
                    .border_color(rgba(colors.accent_outline()))
                    .rounded(px(theme::SCHEMA_KIND_PILL_RADIUS))
                    .px(theme::SCHEMA_KIND_PILL_PADDING_X)
                    .child(relation_kind_pill_text(self.kind)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .ml_auto()
                    .gap(theme::SCHEMA_STATS_GAP)
                    .text_size(px(theme::SCHEMA_STATS_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(stat_label(
                        &format_row_count_stat(self.row_count),
                        "rows",
                        active_theme,
                    ))
                    .child(stat_label(
                        &detail.columns.len().to_string(),
                        "columns",
                        active_theme,
                    ))
                    .child(stat_label(
                        &detail.indexes.len().to_string(),
                        "indexes",
                        active_theme,
                    ))
                    .child(stat_label(
                        &detail.constraints.len().to_string(),
                        "constraints",
                        active_theme,
                    )),
            )
    }

    /// The "Columns" table
    fn render_columns_table(
        &self,
        cx: &mut Context<Self>,
        detail: &RelationSchema,
    ) -> impl IntoElement {
        let active_theme = cx.theme();
        let widths = theme::SCHEMA_COLUMNS_WIDTHS;
        // Type and Default hold the most variable-length content (long type
        // names, long default expressions), so they absorb any width left over
        // once the table fills its section; the rest stay at their fixed width.
        let grows = [true, true, false, true, false];
        let columns = ["Column", "Type", "Null", "Default", "Keys"]
            .iter()
            .zip(widths.iter())
            .zip(grows.iter())
            .map(|((column, &width), &grow)| {
                let table_column = TableColumn::new(
                    width,
                    header_cell(column.to_string(), active_theme).px(cell_x_padding()),
                );
                if grow {
                    table_column.grow()
                } else {
                    table_column
                }
            })
            .collect();

        section(
            "Columns",
            detail.columns.len(),
            Table::new("schema-columns-table", &self.columns_table)
                .style(TableStyle {
                    cell_padding_x: px(0.0),
                    ..schema_table_style(active_theme)
                })
                .columns(columns)
                .row_count(detail.columns.len())
                .rows(Self::render_columns_table_row_cells)
                .vertical_sizing(TableSizing::Fit)
                .render(cx),
            cx.theme(),
        )
    }

    fn render_columns_table_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TableRow> {
        let active_theme = cx.theme();
        let Some(schema) = self.schema() else {
            debug_assert!(
                false,
                "render_columns_table_row_cells called before schema is ready"
            );
            return vec![];
        };

        range
            .map(|ix| {
                let Some(column) = schema.columns.get(ix) else {
                    return TableRow::new(vec![]);
                };
                let cells = vec![
                    div()
                        .when_some(rail_color(column, active_theme), |el, color| {
                            el.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .w(theme::SCHEMA_RAIL_WIDTH)
                                    .bg(rgb(color)),
                            )
                        })
                        .child(
                            div()
                                .text_color(rgb(active_theme.colors.text_primary))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(column.name.clone()),
                        )
                        .px(cell_x_padding())
                        .into_any_element(),
                    div()
                        .px(cell_x_padding())
                        .child(
                            grid::type_tag_accent(&column.type_name, active_theme)
                                .flex_shrink_0()
                                .into_any_element(),
                        )
                        .into_any_element(),
                    null_label(column.nullable, active_theme)
                        .px(cell_x_padding())
                        .into_any_element(),
                    render_default_cell(column.default.as_deref(), active_theme)
                        .px(cell_x_padding())
                        .into_any_element(),
                    div()
                        .px(cell_x_padding())
                        .child(render_keys_cell(column, &schema.constraints, active_theme))
                        .into_any_element(),
                ];

                TableRow::new(cells)
            })
            .collect()
    }

    /// The Indexes table.
    fn render_indexes_table(
        &self,
        cx: &mut Context<Self>,
        detail: &RelationSchema,
    ) -> impl IntoElement {
        let active_theme = cx.theme();
        let widths = theme::SCHEMA_INDEXES_WIDTHS;
        let grows = [false, false, false, true];
        let columns = ["Name", "Method", "Unique", "Definition"]
            .iter()
            .zip(widths.iter())
            .zip(grows.iter())
            .map(|((column, &width), &grow)| {
                let table_column =
                    TableColumn::new(width, header_cell(column.to_string(), active_theme));
                if grow {
                    table_column.grow()
                } else {
                    table_column
                }
            })
            .collect();

        section(
            "Indexes",
            detail.indexes.len(),
            Table::new("schema-indexes-table", &self.indexes_table)
                .style(schema_table_style(active_theme))
                .columns(columns)
                .row_count(detail.indexes.len())
                .rows(Self::render_index_table_row_cells)
                .vertical_sizing(TableSizing::Fit)
                .render(cx),
            cx.theme(),
        )
    }

    fn render_index_table_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TableRow> {
        let colors = cx.theme().colors;
        let Some(schema) = self.schema() else {
            debug_assert!(
                false,
                "render_index_table_row_cells called before schema is ready"
            );
            return vec![];
        };

        range
            .map(|ix| {
                let Some(index) = schema.indexes.get(ix) else {
                    return TableRow::new(vec![]);
                };
                let cells = vec![
                    div()
                        .text_color(rgb(colors.text_primary))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(index.name.clone())
                        .into_any_element(),
                    div()
                        .text_color(rgb(colors.text_secondary))
                        .child(index.method.clone())
                        .into_any_element(),
                    if index.unique {
                        div()
                            .text_color(rgb(colors.value_bool))
                            .child(theme::SCHEMA_INDEX_UNIQUE_LABEL)
                            .into_any_element()
                    } else {
                        div()
                            .text_color(rgb(colors.text_tertiary))
                            .child(theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER)
                            .into_any_element()
                    },
                    div()
                        .text_color(rgb(colors.text_secondary))
                        .child(index.definition.clone())
                        .into_any_element(),
                ];

                TableRow::new(cells)
            })
            .collect()
    }

    /// The Constraints table.
    fn render_constraints_table(
        &self,
        cx: &mut Context<Self>,
        detail: &RelationSchema,
    ) -> impl IntoElement {
        let active_theme = cx.theme();
        let widths = theme::SCHEMA_CONSTRAINTS_WIDTHS;
        let grows = [false, false, true];
        let columns = ["Name", "Type", "Definition"]
            .iter()
            .zip(widths.iter())
            .zip(grows.iter())
            .map(|((column, &width), &grow)| {
                let table_column =
                    TableColumn::new(width, header_cell(column.to_string(), active_theme));
                if grow {
                    table_column.grow()
                } else {
                    table_column
                }
            })
            .collect();

        section(
            "Constraints",
            detail.constraints.len(),
            Table::new("schema-constraints-table", &self.constraints_table)
                .style(schema_table_style(active_theme))
                .columns(columns)
                .row_count(detail.constraints.len())
                .rows(Self::render_constraints_table_row_cells)
                .vertical_sizing(TableSizing::Fit)
                .render(cx),
            cx.theme(),
        )
    }

    fn render_constraints_table_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TableRow> {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let Some(schema) = self.schema() else {
            debug_assert!(
                false,
                "render_constraints_table_row_cells called before schema is ready"
            );
            return vec![];
        };

        range
            .map(|ix| {
                let Some(constraint) = schema.constraints.get(ix) else {
                    return TableRow::new(vec![]);
                };
                let (kind_label, kind_color) = constraint_kind_badge(constraint.kind, active_theme);
                let cells = vec![
                    div()
                        .text_color(rgb(colors.text_primary))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(constraint.name.clone())
                        .into_any_element(),
                    key_badge(kind_label, kind_color, colors.border).into_any_element(),
                    div()
                        .text_color(rgb(colors.text_secondary))
                        .child(constraint.definition.clone())
                        .into_any_element(),
                ];

                TableRow::new(cells)
            })
            .collect()
    }
}

impl Render for SchemaTabView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: gpui::AnyElement = match &self.state {
            FetchState::Loading => Self::render_placeholder(
                cx.theme().colors.text_tertiary,
                "Loading schema...",
                "Fetching structure.",
                cx.theme(),
            )
            .into_any_element(),
            FetchState::Error(message) => Self::render_placeholder(
                cx.theme().colors.status_error,
                "Schema unavailable",
                message,
                cx.theme(),
            )
            .into_any_element(),
            FetchState::Ready(detail) => {
                // The scrolled content: a single non-shrinking column of the
                // three tables. `flex_shrink_0` keeps it at its natural
                // height so it can overflow the page and actually scroll,
                // rather than being squeezed to the viewport.
                let content = div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .p(theme::SCHEMA_SCROLL_PADDING)
                    .gap(theme::SCHEMA_SECTION_GAP)
                    .child(self.render_columns_table(cx, detail))
                    .child(self.render_indexes_table(cx, detail))
                    .child(self.render_constraints_table(cx, detail));
                let page = vertical_scroll(
                    "schema-tab-scroll",
                    &self.page_scroll,
                    ScrollbarStyle::default(),
                    content,
                    cx,
                );
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .flex_1()
                    .child(self.render_head(detail, cx.theme()))
                    .child(page)
                    .into_any_element()
            }
        };

        div()
            .flex()
            .flex_col()
            .min_h_0()
            .flex_1()
            .bg(rgb(cx.theme().colors.bg_app))
            .child(body)
    }
}

/// One header stat, e.g. `~1,240` bolded followed by a faint ` rows` label.
fn stat_label(value: &str, label: &str, active_theme: &Theme) -> impl IntoElement {
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
fn section(
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
fn cell_x_padding() -> gpui::Pixels {
    px(grid::CELL_PADDING_X)
}

fn header_cell(text: String, active_theme: &Theme) -> gpui::Div {
    div()
        .text_color(rgb(active_theme.colors.text_primary))
        .child(text)
}

/// The Null cell's text and color for `nullable`.
fn null_label(nullable: bool, active_theme: &Theme) -> gpui::Div {
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
fn render_default_cell(default: Option<&str>, active_theme: &Theme) -> gpui::Div {
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
fn render_keys_cell(
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
            colors.border,
        )
        .into_any_element(),
        Some(KeyCellBadge::Foreign(target)) => {
            fk_link_chip(&target, active_theme).into_any_element()
        }
    }
}

/// A small outlined badge for the Keys cell (PK, unique, or check).
fn key_badge(label: &str, text_color: u32, border_color: u32) -> gpui::Div {
    div()
        .text_size(px(theme::SCHEMA_BADGE_TEXT_SIZE))
        .text_color(rgb(text_color))
        .border_1()
        .border_color(rgba(border_color))
        .rounded(px(theme::SCHEMA_BADGE_RADIUS))
        .px(theme::SCHEMA_BADGE_PADDING_X)
        .child(label.to_owned())
}

/// The `-> target.column` foreign-key link chip.
fn fk_link_chip(target: &str, active_theme: &Theme) -> gpui::Div {
    let colors = active_theme.colors;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .text_size(px(theme::SCHEMA_FK_CHIP_TEXT_SIZE))
        .font_family(&active_theme.fonts.data)
        .text_color(rgb(colors.key_fk))
        .bg(rgba(colors.fk_wash()))
        .border_1()
        .border_color(rgba(colors.fk_outline()))
        .rounded(px(theme::SCHEMA_FK_CHIP_RADIUS))
        .px(theme::SCHEMA_BADGE_PADDING_X)
        .child(theme::SCHEMA_FK_ARROW)
        .child(target.to_owned())
}

/// The label and text color a [`ConstraintKind`] renders its type badge
/// with.
fn constraint_kind_badge(kind: ConstraintKind, active_theme: &Theme) -> (&'static str, u32) {
    let colors = active_theme.colors;
    match kind {
        ConstraintKind::PrimaryKey => ("PRIMARY KEY", colors.accent),
        ConstraintKind::ForeignKey => ("FOREIGN KEY", colors.key_fk),
        ConstraintKind::Unique => ("UNIQUE", colors.value_bool),
        ConstraintKind::Check => ("CHECK", colors.text_secondary),
    }
}

/// The uppercase kind-pill text for a relation's kind.
fn relation_kind_pill_text(kind: RelationKind) -> &'static str {
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
fn format_row_count_stat(row_count: Option<RowCount>) -> String {
    row_count.map_or_else(
        || theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER.to_owned(),
        RowCount::grouped_display,
    )
}

/// The left key-rail tick color for `column`: teal for a primary key, the
/// link hue for a foreign key, or none.
fn rail_color(column: &ColumnDetail, active_theme: &Theme) -> Option<u32> {
    if column.is_primary_key {
        Some(active_theme.colors.accent)
    } else if column.foreign_key.is_some() {
        Some(active_theme.colors.key_fk)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use zsql_core::{ColumnDetail, ForeignKeyRef};

    use super::{rail_color, relation_kind_pill_text};
    use zsql_ui::theme::Theme;

    fn plain_column() -> ColumnDetail {
        ColumnDetail {
            name: "n".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
            default: None,
            is_primary_key: false,
            is_unique: false,
            foreign_key: None,
        }
    }

    #[test]
    fn rail_color_prefers_primary_key_over_foreign_key() {
        let column = ColumnDetail {
            is_primary_key: true,
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        let theme = Theme::default();
        assert_eq!(rail_color(&column, &theme), Some(theme.colors.accent));
    }

    #[test]
    fn rail_color_is_link_hue_for_a_foreign_key_only_column() {
        let column = ColumnDetail {
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        let theme = Theme::default();
        assert_eq!(rail_color(&column, &theme), Some(theme.colors.key_fk));
    }

    #[test]
    fn rail_color_is_none_for_a_plain_column() {
        assert_eq!(rail_color(&plain_column(), &Theme::default()), None);
    }

    #[test]
    fn relation_kind_pill_text_maps_every_kind() {
        use zsql_core::RelationKind;
        assert_eq!(relation_kind_pill_text(RelationKind::Table), "TABLE");
        assert_eq!(relation_kind_pill_text(RelationKind::View), "VIEW");
        assert_eq!(
            relation_kind_pill_text(RelationKind::MatView),
            "MATERIALIZED VIEW"
        );
        assert_eq!(
            relation_kind_pill_text(RelationKind::Partitioned),
            "PARTITIONED TABLE"
        );
    }
}

#[cfg(test)]
mod render_tests {
    use gpui::{AppContext as _, TestAppContext};
    use zsql_core::{
        ColumnDetail, ConstraintInfo, ConstraintKind, ForeignKeyRef, IndexInfo, RelationKind,
        RelationSchema,
    };

    use super::SchemaTabView;
    use crate::session::Session;

    fn sample_detail() -> RelationSchema {
        RelationSchema {
            columns: vec![
                ColumnDetail {
                    name: "id".to_owned(),
                    type_name: "int8".to_owned(),
                    nullable: false,
                    default: Some("nextval('orders_id_seq')".to_owned()),
                    is_primary_key: true,
                    is_unique: false,
                    foreign_key: None,
                },
                ColumnDetail {
                    name: "user_id".to_owned(),
                    type_name: "int8".to_owned(),
                    nullable: false,
                    default: None,
                    is_primary_key: false,
                    is_unique: false,
                    foreign_key: Some(ForeignKeyRef {
                        schema: "public".to_owned(),
                        table: "users".to_owned(),
                        columns: vec!["id".to_owned()],
                    }),
                },
                ColumnDetail {
                    name: "email".to_owned(),
                    type_name: "text".to_owned(),
                    nullable: false,
                    default: None,
                    is_primary_key: false,
                    is_unique: true,
                    foreign_key: None,
                },
                ColumnDetail {
                    name: "total_cents".to_owned(),
                    type_name: "int4".to_owned(),
                    nullable: false,
                    default: Some("0".to_owned()),
                    is_primary_key: false,
                    is_unique: false,
                    foreign_key: None,
                },
            ],
            indexes: vec![IndexInfo {
                name: "orders_pkey".to_owned(),
                method: "btree".to_owned(),
                unique: true,
                definition: "CREATE UNIQUE INDEX orders_pkey ON orders USING btree (id)".to_owned(),
            }],
            constraints: vec![
                ConstraintInfo {
                    name: "orders_pkey".to_owned(),
                    kind: ConstraintKind::PrimaryKey,
                    definition: "PRIMARY KEY (id)".to_owned(),
                },
                ConstraintInfo {
                    name: "orders_total_cents_nonneg".to_owned(),
                    kind: ConstraintKind::Check,
                    definition: "total_cents >= 0".to_owned(),
                },
            ],
        }
    }

    /// A connection double whose `describe_relation` resolves immediately
    /// with `outcome`.
    struct FakeConnection {
        outcome: Result<RelationSchema, String>,
    }

    #[async_trait::async_trait]
    impl zsql_core::Connection for FakeConnection {
        fn stream_query(
            &self,
            _sql: String,
            _sink: zsql_core::BatchSink,
        ) -> zsql_core::QueryHandle {
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            zsql_core::QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<zsql_core::SchemaTree, zsql_core::CoreError> {
            Ok(zsql_core::SchemaTree::default())
        }

        async fn ping(&self) -> Result<(), zsql_core::CoreError> {
            Ok(())
        }

        async fn count_rows(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<zsql_core::RowCount, zsql_core::CoreError> {
            Ok(zsql_core::RowCount::Exact(1_240))
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<RelationSchema, zsql_core::CoreError> {
            self.outcome
                .clone()
                .map_err(zsql_core::CoreError::introspection)
        }
    }

    /// A session connected to a [`FakeConnection`] whose `describe_relation`
    /// resolves to `outcome`.
    fn session_for(
        cx: &mut TestAppContext,
        outcome: Result<RelationSchema, String>,
    ) -> gpui::Entity<Session> {
        let connection: std::sync::Arc<dyn zsql_core::Connection> =
            std::sync::Arc::new(FakeConnection { outcome });
        cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)))
    }

    #[gpui::test]
    fn renders_a_populated_relation_schema_without_panicking(cx: &mut TestAppContext) {
        let session = session_for(cx, Ok(sample_detail()));
        let (_view, vcx) = cx.add_window_view(|_window, cx| {
            SchemaTabView::new(
                &session,
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                cx,
            )
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn renders_an_error_state_without_panicking(cx: &mut TestAppContext) {
        let session = session_for(cx, Err("relation not found: public.ghost".to_owned()));
        let (_view, vcx) = cx.add_window_view(|_window, cx| {
            SchemaTabView::new(
                &session,
                "public".to_owned(),
                "ghost".to_owned(),
                RelationKind::Table,
                cx,
            )
        });
        vcx.run_until_parked();
    }
}
