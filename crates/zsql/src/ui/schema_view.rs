//! The read-only schema tab: fetches a relation's full structural detail
//! (columns, indexes, constraints) on construction and renders it once
//! ready, reusing `zsql_ui::grid` primitives and the results grid's
//! type-tag treatment.

use gpui::{Context, Entity, Render, Window, div, prelude::*, px, rgb, rgba};
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, ForeignKeyRef, IndexInfo, KeyBadge, RelationKind,
    RelationSchema, RowCount,
};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::{colors, grid};

use super::results::group_thousands;
use super::theme;
use crate::session::Session;

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
    fn render_placeholder(color: u32, title: &str, detail: &str) -> impl IntoElement {
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
                    .text_color(rgb(colors::FAINT))
                    .child(detail.to_owned()),
            )
    }

    /// The header meta strip: structure icon, qualified name, kind pill, and
    /// the four header counts.
    fn render_head(&self, detail: &RelationSchema) -> impl IntoElement {
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
            .border_color(rgb(colors::LINE))
            .font_family("monospace")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(icon(
                        IconName::Table,
                        px(theme::SCHEMA_TITLE_TEXT_SIZE),
                        colors::TEAL,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::SCHEMA_TITLE_TEXT_SIZE))
                            .text_color(rgb(colors::FAINT))
                            .child(format!("{}.", self.schema)),
                    )
                    .child(
                        div()
                            .text_size(px(theme::SCHEMA_TITLE_TEXT_SIZE))
                            .text_color(rgb(colors::TEXT))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.relation.clone()),
                    ),
            )
            .child(
                div()
                    .text_size(px(theme::SCHEMA_KIND_PILL_TEXT_SIZE))
                    .text_color(rgb(colors::TEAL))
                    .border_1()
                    .border_color(rgba(theme::SCHEMA_KIND_PILL_BORDER))
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
                    .text_color(rgb(colors::FAINT))
                    .child(stat_label(&format_row_count_stat(self.row_count), "rows"))
                    .child(stat_label(&detail.columns.len().to_string(), "columns"))
                    .child(stat_label(&detail.indexes.len().to_string(), "indexes"))
                    .child(stat_label(
                        &detail.constraints.len().to_string(),
                        "constraints",
                    )),
            )
    }

    /// The Columns table: the hero of the schema view.
    fn render_columns_table(detail: &RelationSchema) -> impl IntoElement {
        let widths = theme::SCHEMA_COLUMNS_WIDTHS;
        section(
            "Columns",
            detail.columns.len(),
            table_shell()
                .child(header_row(
                    widths,
                    ["Column", "Type", "Null", "Default", "Keys"],
                ))
                .children(
                    detail
                        .columns
                        .iter()
                        .map(|column| render_column_row(column, &detail.constraints, widths)),
                ),
        )
    }

    /// The Indexes table.
    fn render_indexes_table(detail: &RelationSchema) -> impl IntoElement {
        let widths = theme::SCHEMA_INDEXES_WIDTHS;
        section(
            "Indexes",
            detail.indexes.len(),
            table_shell()
                .child(header_row(
                    widths,
                    ["Name", "Method", "Unique", "Definition"],
                ))
                .children(
                    detail
                        .indexes
                        .iter()
                        .map(|index| render_index_row(index, widths)),
                ),
        )
    }

    /// The Constraints table.
    fn render_constraints_table(detail: &RelationSchema) -> impl IntoElement {
        let widths = theme::SCHEMA_CONSTRAINTS_WIDTHS;
        section(
            "Constraints",
            detail.constraints.len(),
            table_shell()
                .child(header_row(widths, ["Name", "Type", "Definition"]))
                .children(
                    detail
                        .constraints
                        .iter()
                        .map(|constraint| render_constraint_row(constraint, widths)),
                ),
        )
    }
}

impl Render for SchemaTabView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body: gpui::AnyElement = match &self.state {
            FetchState::Loading => {
                Self::render_placeholder(colors::FAINT, "Loading schema...", "Fetching structure.")
                    .into_any_element()
            }
            FetchState::Error(message) => {
                Self::render_placeholder(theme::STATUS_ERROR, "Schema unavailable", message)
                    .into_any_element()
            }
            FetchState::Ready(detail) => div()
                .flex()
                .flex_col()
                .min_h_0()
                .flex_1()
                .child(self.render_head(detail))
                .child(
                    div()
                        .id("schema-tab-scroll")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p(theme::SCHEMA_SCROLL_PADDING)
                        .gap(theme::SCHEMA_SECTION_GAP)
                        .child(Self::render_columns_table(detail))
                        .child(Self::render_indexes_table(detail))
                        .child(Self::render_constraints_table(detail)),
                )
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .min_h_0()
            .flex_1()
            .bg(rgb(colors::INK))
            .child(body)
    }
}

/// One header stat, e.g. `~1,240` bolded followed by a faint ` rows` label.
fn stat_label(value: &str, label: &str) -> impl IntoElement {
    div()
        .child(format!("{value} "))
        .text_color(rgb(colors::MUTED))
        .child(div().text_color(rgb(colors::FAINT)).child(label.to_owned()))
}

/// A section: an uppercase label with a trailing count pill, followed by
/// `table`.
fn section(label: &str, count: usize, table: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .mb(theme::SCHEMA_SECTION_LABEL_MARGIN_BOTTOM)
                .text_size(px(theme::SCHEMA_SECTION_LABEL_TEXT_SIZE))
                .text_color(rgb(colors::FAINT))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(label.to_uppercase())
                .child(
                    div()
                        .text_color(rgb(colors::MUTED))
                        .border_1()
                        .border_color(rgb(colors::LINE))
                        .rounded(px(theme::SCHEMA_SECTION_COUNT_PILL_RADIUS))
                        .px(theme::SCHEMA_SECTION_COUNT_PILL_PADDING_X)
                        .child(count.to_string()),
                ),
        )
        .child(table)
}

/// Shared chrome for one of the schema view's tables.
fn table_shell() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .border_1()
        .border_color(rgb(colors::LINE_SOFT))
        .rounded(px(theme::SCHEMA_TABLE_RADIUS))
        .overflow_hidden()
}

/// A header row of `labels`, each cell sized to its matching entry in
/// `widths`. The trailing cell drops its vertical separator so the table's
/// own border carries the right edge.
fn header_row<const N: usize>(widths: [gpui::Pixels; N], labels: [&str; N]) -> impl IntoElement {
    let mut row = div()
        .flex()
        .flex_row()
        .bg(rgb(colors::RAISE))
        .border_b_1()
        .border_color(rgb(colors::LINE));
    let last = N.saturating_sub(1);
    for (index, (width, label)) in widths.into_iter().zip(labels).enumerate() {
        let mut cell = grid::header_cell_shell(width)
            .text_size(px(theme::SCHEMA_TABLE_HEADER_TEXT_SIZE))
            .text_color(rgb(colors::MUTED))
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(label.to_owned());
        if index == last {
            cell = cell.border_r_0();
        }
        row = row.child(cell);
    }
    row
}

/// The Type cell: the teal type tag shown in full, never clipped mid-glyph
/// even for a long Postgres type name.
fn type_cell(width: gpui::Pixels, type_name: &str) -> gpui::Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h_full()
        .px(px(grid::CELL_PADDING_X))
        .border_r_1()
        .border_color(rgb(colors::LINE_SOFT))
        .child(grid::type_tag(type_name).flex_shrink_0())
}

/// One row of the Columns table: the left key-rail tick, then Column/Type/
/// Null/Default/Keys cells.
fn render_column_row(
    column: &ColumnDetail,
    constraints: &[ConstraintInfo],
    widths: [gpui::Pixels; 5],
) -> impl IntoElement {
    let [name_w, type_w, null_w, default_w, keys_w] = widths;
    div()
        .flex()
        .flex_row()
        .border_b_1()
        .border_color(rgb(colors::LINE_SOFT))
        .child(
            grid::body_cell_shell(name_w)
                .relative()
                .when_some(rail_color(column), |el, color| {
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
                        .text_color(rgb(colors::TEXT))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(column.name.clone()),
                ),
        )
        .child(type_cell(type_w, &column.type_name))
        .child(grid::body_cell_shell(null_w).child(null_label(column.nullable)))
        .child(
            grid::body_cell_shell(default_w).child(render_default_cell(column.default.as_deref())),
        )
        .child(
            grid::body_cell_shell(keys_w)
                .border_r_0()
                .child(render_keys_cell(column, constraints)),
        )
}

/// The Null cell's text and color for `nullable`.
fn null_label(nullable: bool) -> gpui::Div {
    if nullable {
        div()
            .italic()
            .text_color(rgb(colors::FAINT))
            .child(theme::SCHEMA_NULLABLE_LABEL)
    } else {
        div()
            .text_color(rgb(colors::MUTED))
            .child(theme::SCHEMA_NOT_NULL_LABEL)
    }
}

/// The Default cell: violet for a function call, amber for a literal, a
/// faint dash placeholder for none.
fn render_default_cell(default: Option<&str>) -> gpui::Div {
    match classify_default(default) {
        DefaultKind::None => div()
            .text_color(rgb(colors::FAINT))
            .child(theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER),
        DefaultKind::Literal => div()
            .text_color(rgb(colors::BOOL))
            .child(default.unwrap_or_default().to_owned()),
        DefaultKind::Function => div()
            .text_color(rgb(colors::NUMBER))
            .child(default.unwrap_or_default().to_owned()),
    }
}

/// The Keys cell: a PK/unique/check badge, an FK link chip, or nothing.
fn render_keys_cell(column: &ColumnDetail, constraints: &[ConstraintInfo]) -> gpui::AnyElement {
    match key_cell_badge(column, constraints) {
        None => div().into_any_element(),
        Some(KeyCellBadge::Primary) => key_badge(
            theme::SCHEMA_BADGE_PK_LABEL,
            colors::TEAL,
            theme::SCHEMA_BADGE_PK_BORDER,
        )
        .into_any_element(),
        Some(KeyCellBadge::Unique) => key_badge(
            theme::SCHEMA_BADGE_UNIQUE_LABEL,
            colors::BOOL,
            theme::SCHEMA_BADGE_UNIQUE_BORDER,
        )
        .into_any_element(),
        Some(KeyCellBadge::Check) => {
            key_badge(theme::SCHEMA_BADGE_CHECK_LABEL, colors::MUTED, colors::LINE)
                .into_any_element()
        }
        Some(KeyCellBadge::Foreign(target)) => fk_link_chip(&target).into_any_element(),
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
fn fk_link_chip(target: &str) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .text_size(px(theme::SCHEMA_FK_CHIP_TEXT_SIZE))
        .font_family("monospace")
        .text_color(rgb(colors::LINK))
        .bg(rgba(theme::SCHEMA_BADGE_LINK_BG))
        .border_1()
        .border_color(rgba(theme::SCHEMA_BADGE_LINK_BORDER))
        .rounded(px(theme::SCHEMA_FK_CHIP_RADIUS))
        .px(theme::SCHEMA_BADGE_PADDING_X)
        .child(theme::SCHEMA_FK_ARROW)
        .child(target.to_owned())
}

/// One row of the Indexes table.
fn render_index_row(index: &IndexInfo, widths: [gpui::Pixels; 4]) -> impl IntoElement {
    let [name_w, method_w, unique_w, def_w] = widths;
    div()
        .flex()
        .flex_row()
        .border_b_1()
        .border_color(rgb(colors::LINE_SOFT))
        .child(
            grid::body_cell_shell(name_w).child(
                div()
                    .text_color(rgb(colors::TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(index.name.clone()),
            ),
        )
        .child(
            grid::body_cell_shell(method_w).child(
                div()
                    .text_color(rgb(colors::MUTED))
                    .child(index.method.clone()),
            ),
        )
        .child(grid::body_cell_shell(unique_w).child(if index.unique {
            div()
                .text_color(rgb(colors::BOOL))
                .child(theme::SCHEMA_INDEX_UNIQUE_LABEL)
        } else {
            div()
                .text_color(rgb(colors::FAINT))
                .child(theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER)
        }))
        .child(
            grid::body_cell_shell(def_w).border_r_0().child(
                div()
                    .text_color(rgb(colors::MUTED))
                    .child(index.definition.clone()),
            ),
        )
}

/// One row of the Constraints table.
fn render_constraint_row(
    constraint: &ConstraintInfo,
    widths: [gpui::Pixels; 3],
) -> impl IntoElement {
    let [name_w, kind_w, def_w] = widths;
    let (kind_label, kind_color) = constraint_kind_badge(constraint.kind);
    div()
        .flex()
        .flex_row()
        .border_b_1()
        .border_color(rgb(colors::LINE_SOFT))
        .child(
            grid::body_cell_shell(name_w).child(
                div()
                    .text_color(rgb(colors::TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(constraint.name.clone()),
            ),
        )
        .child(grid::body_cell_shell(kind_w).child(key_badge(kind_label, kind_color, colors::LINE)))
        .child(
            grid::body_cell_shell(def_w).border_r_0().child(
                div()
                    .text_color(rgb(colors::MUTED))
                    .child(constraint.definition.clone()),
            ),
        )
}

/// The label and text color a [`ConstraintKind`] renders its type badge
/// with.
fn constraint_kind_badge(kind: ConstraintKind) -> (&'static str, u32) {
    match kind {
        ConstraintKind::PrimaryKey => ("PRIMARY KEY", colors::TEAL),
        ConstraintKind::ForeignKey => ("FOREIGN KEY", colors::LINK),
        ConstraintKind::Unique => ("UNIQUE", colors::BOOL),
        ConstraintKind::Check => ("CHECK", colors::MUTED),
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
    let Some(row_count) = row_count else {
        return theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER.to_owned();
    };
    let grouped = group_thousands(row_count.value());
    if row_count.is_estimated() {
        format!("{}{grouped}", zsql_core::ESTIMATE_MARKER)
    } else {
        grouped
    }
}

/// Classification of a column's `default` expression, for coloring the
/// Default cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultKind {
    /// A function call, e.g. `now()` or `nextval('orders_id_seq')`.
    Function,
    /// A literal value, e.g. `0` or `'pending'`.
    Literal,
    /// No default.
    None,
}

/// Classify `default` for [`DefaultKind`]-based coloring. A value is
/// classified as a function call when it both contains `(` and ends with
/// `)`; any other non-empty value is a literal.
fn classify_default(default: Option<&str>) -> DefaultKind {
    let Some(text) = default else {
        return DefaultKind::None;
    };
    let trimmed = text.trim();
    if trimmed.contains('(') && trimmed.ends_with(')') {
        DefaultKind::Function
    } else {
        DefaultKind::Literal
    }
}

/// Which badge (if any) a column's Keys cell renders, in a fixed priority:
/// primary key, then foreign key, then unique, then check.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyCellBadge {
    /// The column is (part of) the primary key.
    Primary,
    /// The column is a foreign key targeting the carried `table.column(s)`
    /// string.
    Foreign(String),
    /// The column is constrained unique.
    Unique,
    /// The column is mentioned by a `CHECK` constraint.
    Check,
}

/// Classify `column`'s Keys-cell badge from its own flags plus, for the
/// `CHECK` case only, whether any of `constraints` mentions its name.
fn key_cell_badge(column: &ColumnDetail, constraints: &[ConstraintInfo]) -> Option<KeyCellBadge> {
    // The PK/FK/Unique roles come from zsql_core's shared classifier, in its
    // priority order, so the schema view and any other consumer agree on a
    // column's key role. The Check case is layered on top here because it is
    // derived from the relation's constraints, not the column itself.
    if let Some(badge) = column.key_badges().first() {
        return Some(match badge {
            KeyBadge::Primary => KeyCellBadge::Primary,
            KeyBadge::Foreign => KeyCellBadge::Foreign(foreign_key_target(
                column
                    .foreign_key
                    .as_ref()
                    .expect("a Foreign key badge implies the column carries a foreign key"),
            )),
            KeyBadge::Unique => KeyCellBadge::Unique,
        });
    }
    if column_has_check(&column.name, constraints) {
        return Some(KeyCellBadge::Check);
    }
    None
}

/// The `-> target.column` link chip's target string: `table.col1,col2` for
/// a composite key, `table.col` for a single-column one.
fn foreign_key_target(fk: &ForeignKeyRef) -> String {
    format!("{}.{}", fk.table, fk.columns.join(","))
}

/// Whether any `CHECK` constraint in `constraints` mentions `column_name` as
/// a whole identifier (not merely as a substring of a longer name).
fn column_has_check(column_name: &str, constraints: &[ConstraintInfo]) -> bool {
    constraints
        .iter()
        .filter(|constraint| constraint.kind == ConstraintKind::Check)
        .any(|constraint| definition_mentions_column(&constraint.definition, column_name))
}

/// Whether `definition` mentions `column_name` as a whole identifier token,
/// splitting on any character that cannot appear inside a SQL identifier.
fn definition_mentions_column(definition: &str, column_name: &str) -> bool {
    definition
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .any(|token| token == column_name)
}

/// The left key-rail tick color for `column`: teal for a primary key, the
/// link hue for a foreign key, or none.
fn rail_color(column: &ColumnDetail) -> Option<u32> {
    if column.is_primary_key {
        Some(colors::TEAL)
    } else if column.foreign_key.is_some() {
        Some(colors::LINK)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use zsql_core::{ColumnDetail, ConstraintInfo, ConstraintKind, ForeignKeyRef};

    use super::{
        DefaultKind, KeyCellBadge, classify_default, column_has_check, foreign_key_target,
        key_cell_badge, rail_color, relation_kind_pill_text,
    };

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
    fn classify_default_none_for_no_default() {
        assert_eq!(classify_default(None), DefaultKind::None);
    }

    #[test]
    fn classify_default_recognizes_function_calls() {
        assert_eq!(classify_default(Some("now()")), DefaultKind::Function);
        assert_eq!(
            classify_default(Some("nextval('orders_id_seq')")),
            DefaultKind::Function
        );
    }

    #[test]
    fn classify_default_recognizes_literals() {
        assert_eq!(classify_default(Some("0")), DefaultKind::Literal);
        assert_eq!(classify_default(Some("'pending'")), DefaultKind::Literal);
        assert_eq!(classify_default(Some("'{}'::jsonb")), DefaultKind::Literal);
    }

    #[test]
    fn foreign_key_target_joins_a_single_column() {
        let fk = ForeignKeyRef {
            schema: "public".to_owned(),
            table: "users".to_owned(),
            columns: vec!["id".to_owned()],
        };
        assert_eq!(foreign_key_target(&fk), "users.id");
    }

    #[test]
    fn foreign_key_target_joins_a_composite_key() {
        let fk = ForeignKeyRef {
            schema: "public".to_owned(),
            table: "grants".to_owned(),
            columns: vec!["tenant_id".to_owned(), "user_id".to_owned()],
        };
        assert_eq!(foreign_key_target(&fk), "grants.tenant_id,user_id");
    }

    #[test]
    fn key_cell_badge_prioritizes_primary_key_over_everything_else() {
        let column = ColumnDetail {
            is_primary_key: true,
            is_unique: true,
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        assert_eq!(key_cell_badge(&column, &[]), Some(KeyCellBadge::Primary));
    }

    #[test]
    fn key_cell_badge_prefers_foreign_key_over_unique() {
        let column = ColumnDetail {
            is_unique: true,
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        assert_eq!(
            key_cell_badge(&column, &[]),
            Some(KeyCellBadge::Foreign("users.id".to_owned()))
        );
    }

    #[test]
    fn key_cell_badge_is_unique_for_a_unique_only_column() {
        let column = ColumnDetail {
            is_unique: true,
            ..plain_column()
        };
        assert_eq!(key_cell_badge(&column, &[]), Some(KeyCellBadge::Unique));
    }

    #[test]
    fn key_cell_badge_is_check_when_a_check_constraint_mentions_the_column() {
        let column = ColumnDetail {
            name: "total_cents".to_owned(),
            ..plain_column()
        };
        let constraints = [ConstraintInfo {
            name: "orders_total_cents_nonneg".to_owned(),
            kind: ConstraintKind::Check,
            definition: "total_cents >= 0".to_owned(),
        }];
        assert_eq!(
            key_cell_badge(&column, &constraints),
            Some(KeyCellBadge::Check)
        );
    }

    #[test]
    fn key_cell_badge_does_not_match_a_column_name_as_a_mere_substring() {
        let column = ColumnDetail {
            name: "total".to_owned(),
            ..plain_column()
        };
        let constraints = [ConstraintInfo {
            name: "orders_total_cents_nonneg".to_owned(),
            kind: ConstraintKind::Check,
            definition: "total_cents >= 0".to_owned(),
        }];
        assert_eq!(key_cell_badge(&column, &constraints), None);
    }

    #[test]
    fn key_cell_badge_is_none_for_a_plain_column() {
        assert_eq!(key_cell_badge(&plain_column(), &[]), None);
    }

    #[test]
    fn column_has_check_ignores_non_check_constraints() {
        let column_name = "id";
        let constraints = [ConstraintInfo {
            name: "orders_pkey".to_owned(),
            kind: ConstraintKind::PrimaryKey,
            definition: "id".to_owned(),
        }];
        assert!(!column_has_check(column_name, &constraints));
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
        assert_eq!(rail_color(&column), Some(zsql_ui::colors::TEAL));
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
        assert_eq!(rail_color(&column), Some(zsql_ui::colors::LINK));
    }

    #[test]
    fn rail_color_is_none_for_a_plain_column() {
        assert_eq!(rail_color(&plain_column()), None);
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
                .map_err(zsql_core::CoreError::Introspection)
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
