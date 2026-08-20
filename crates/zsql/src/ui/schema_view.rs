//! The read-only schema tab: fetches a relation's full structural detail
//! (columns, indexes, constraints) on construction and renders it once
//! ready, reusing `zsql_ui::grid` primitives and the results grid's
//! type-tag treatment.

use std::ops::Range;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, FocusHandle, FontWeight, Render, Window,
    actions, div, prelude::*, px, rgb,
};
use zsql_core::{
    ColumnDetail, ConstraintInfo, KeyCellBadge, RelationKind, RelationSchema, RowCount,
    key_cell_badge,
};
use zsql_ui::grid;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{ScrollView, ScrollbarStyle, vertical_scroll};
use zsql_ui::table::{Table, TableColumn, TableRow, TableSizing, TableState, TableStyle};
use zsql_ui::theme::{ActiveTheme, Theme};

mod bindings;
mod cells;

pub(crate) use bindings::SchemaViewBindings;

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
    /// State for the "Columns" table
    columns_table: Entity<TableState>,
    /// State for the "Indexes" table
    indexes_table: Entity<TableState>,
    /// State for the "Constraints" table
    constraints_table: Entity<TableState>,
    /// Vertical scroll for the whole tab body: the three `Fit` tables never
    /// scroll themselves, so the page scrolls all of them together.
    page_scroll: ScrollView,
    /// Focus handle for selecting table elements
    focus_handle: FocusHandle,
}

actions!(zsql_schema_view, [Copy]);

const BINDING_CONTEXT: &str = "schema-tab-vie";

/// Register the schema view's key bindings from `bindings`. Call on startup.
pub fn init(cx: &mut App, bindings: &SchemaViewBindings) {
    let mut keys = Vec::new();
    crate::keybindings::bind_all(&mut keys, &bindings.copy, &Copy, BINDING_CONTEXT);
    let registered = keys.len();
    cx.bind_keys(keys);
    tracing::debug!(registered, "schema view keybindings registered");
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
            focus_handle: cx.focus_handle(),
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
                    .border_color(colors.accent_outline())
                    .rounded(px(theme::SCHEMA_KIND_PILL_RADIUS))
                    .px(theme::SCHEMA_KIND_PILL_PADDING_X)
                    .child(cells::relation_kind_pill_text(self.kind)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .ml_auto()
                    .gap(theme::SCHEMA_STATS_GAP)
                    .text_size(px(theme::SCHEMA_STATS_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(cells::stat_label(
                        &cells::format_row_count_stat(self.row_count),
                        "rows",
                        active_theme,
                    ))
                    .child(cells::stat_label(
                        &detail.columns.len().to_string(),
                        "columns",
                        active_theme,
                    ))
                    .child(cells::stat_label(
                        &detail.indexes.len().to_string(),
                        "indexes",
                        active_theme,
                    ))
                    .child(cells::stat_label(
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
                    cells::header_cell(column.to_string(), active_theme)
                        .px(cells::cell_x_padding()),
                );
                if grow {
                    table_column.grow()
                } else {
                    table_column
                }
            })
            .collect();

        cells::section(
            "Columns",
            detail.columns.len(),
            Table::new("schema-columns-table", &self.columns_table)
                .style(TableStyle {
                    cell_padding_x: px(0.0),
                    ..cells::schema_table_style(active_theme)
                })
                .columns(columns)
                .row_count(detail.columns.len())
                .rows(Self::render_columns_table_row_cells)
                .vertical_sizing(TableSizing::Fit)
                .selectable()
                .focus_on_cell_click(self.focus_handle.clone())
                .on_cell_click(Self::create_unfocus_handler(false, true, true))
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
                let cells = Self::columns_table_cells(column, schema, active_theme)
                    .into_iter()
                    .map(|(cell, _value)| cell)
                    .collect();
                TableRow::new(cells)
            })
            .collect()
    }

    fn columns_table_cells(
        column: &ColumnDetail,
        schema: &RelationSchema,
        active_theme: &Theme,
    ) -> Vec<(AnyElement, String)> {
        let column_name = div()
            .when_some(cells::rail_color(column, active_theme), |el, color| {
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
            .px(cells::cell_x_padding())
            .into_any_element();
        let type_name = div()
            .px(cells::cell_x_padding())
            .child(
                grid::type_tag_accent(&column.type_name, active_theme)
                    .flex_shrink_0()
                    .into_any_element(),
            )
            .into_any_element();
        let null_label = cells::null_label(column.nullable, active_theme)
            .px(cells::cell_x_padding())
            .into_any_element();
        let default_cell = cells::render_default_cell(column.default.as_deref(), active_theme)
            .px(cells::cell_x_padding())
            .into_any_element();
        let keys_cell = div()
            .px(cells::cell_x_padding())
            .child(cells::render_keys_cell(
                column,
                &schema.constraints,
                active_theme,
            ))
            .into_any_element();
        vec![
            (column_name, column.name.clone()),
            (type_name, column.type_name.clone()),
            (
                null_label,
                if column.nullable {
                    theme::SCHEMA_NULLABLE_LABEL.to_string()
                } else {
                    theme::SCHEMA_NOT_NULL_LABEL.to_string()
                },
            ),
            (default_cell, column.default.clone().unwrap_or_default()),
            (
                keys_cell,
                key_cell_badge(column, &schema.constraints).map_or_else(String::new, |badge| {
                    match badge {
                        KeyCellBadge::Primary => theme::SCHEMA_BADGE_PK_LABEL.to_string(),
                        KeyCellBadge::Unique => theme::SCHEMA_BADGE_UNIQUE_LABEL.to_string(),
                        KeyCellBadge::Check => theme::SCHEMA_BADGE_CHECK_LABEL.to_string(),
                        KeyCellBadge::Foreign(target) => format!("-> {target}"),
                    }
                }),
            ),
        ]
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
                    TableColumn::new(width, cells::header_cell(column.to_string(), active_theme));
                if grow {
                    table_column.grow()
                } else {
                    table_column
                }
            })
            .collect();

        cells::section(
            "Indexes",
            detail.indexes.len(),
            Table::new("schema-indexes-table", &self.indexes_table)
                .style(cells::schema_table_style(active_theme))
                .columns(columns)
                .row_count(detail.indexes.len())
                .rows(Self::render_index_table_row_cells)
                .vertical_sizing(TableSizing::Fit)
                .selectable()
                .focus_on_cell_click(self.focus_handle.clone())
                .on_cell_click(Self::create_unfocus_handler(true, false, true))
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
                let cells = Self::indexes_table_cells(index, cx.theme())
                    .into_iter()
                    .map(|(cell, _value)| cell)
                    .collect();
                TableRow::new(cells)
            })
            .collect()
    }

    fn indexes_table_cells(
        index: &zsql_core::IndexInfo,
        active_theme: &Theme,
    ) -> Vec<(AnyElement, String)> {
        let colors = active_theme.colors;
        let name = div()
            .text_color(rgb(colors.text_primary))
            .font_weight(FontWeight::SEMIBOLD)
            .child(index.name.clone())
            .into_any_element();
        let method = div()
            .text_color(rgb(colors.text_secondary))
            .child(index.method.clone())
            .into_any_element();
        let unique = if index.unique {
            (
                div()
                    .text_color(rgb(colors.value_bool))
                    .child(theme::SCHEMA_INDEX_UNIQUE_LABEL)
                    .into_any_element(),
                theme::SCHEMA_INDEX_UNIQUE_LABEL.to_string(),
            )
        } else {
            (
                div()
                    .text_color(rgb(colors.text_tertiary))
                    .child(theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER)
                    .into_any_element(),
                theme::SCHEMA_DEFAULT_NONE_PLACEHOLDER.to_string(),
            )
        };
        let definition = div()
            .text_color(rgb(colors.text_secondary))
            .child(index.definition.clone())
            .into_any_element();
        vec![
            (name, index.name.clone()),
            (method, index.method.clone()),
            unique,
            (definition, index.definition.clone()),
        ]
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
                    TableColumn::new(width, cells::header_cell(column.to_string(), active_theme));
                if grow {
                    table_column.grow()
                } else {
                    table_column
                }
            })
            .collect();

        cells::section(
            "Constraints",
            detail.constraints.len(),
            Table::new("schema-constraints-table", &self.constraints_table)
                .style(cells::schema_table_style(active_theme))
                .columns(columns)
                .row_count(detail.constraints.len())
                .rows(Self::render_constraints_table_row_cells)
                .vertical_sizing(TableSizing::Fit)
                .selectable()
                .on_cell_click(Self::create_unfocus_handler(true, true, false))
                .focus_on_cell_click(self.focus_handle.clone())
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
                let cells = Self::constraints_table_cells(constraint, active_theme)
                    .into_iter()
                    .map(|(cell, _value)| cell)
                    .collect();
                TableRow::new(cells)
            })
            .collect()
    }

    fn constraints_table_cells(
        constraint: &ConstraintInfo,
        active_theme: &Theme,
    ) -> Vec<(AnyElement, String)> {
        let colors = active_theme.colors;
        let (kind_label, kind_color) = cells::constraint_kind_badge(constraint.kind, active_theme);
        vec![
            (
                div()
                    .text_color(rgb(colors.text_primary))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(constraint.name.clone())
                    .into_any_element(),
                constraint.name.clone(),
            ),
            (
                cells::key_badge(kind_label, kind_color, rgb(colors.border)).into_any_element(),
                kind_label.to_string(),
            ),
            (
                div()
                    .text_color(rgb(colors.text_secondary))
                    .child(constraint.definition.clone())
                    .into_any_element(),
                constraint.definition.clone(),
            ),
        ]
    }

    fn create_unfocus_handler(
        unfocus_columns: bool,
        unfocus_indexes: bool,
        unfocus_constraints: bool,
    ) -> impl Fn(&mut SchemaTabView, usize, usize, &mut Window, &mut Context<SchemaTabView>) + 'static
    {
        let handler = |state: &mut TableState, cx: &mut Context<TableState>| {
            state.clear_focused_cell();
            cx.notify();
        };
        move |view, _row, _col, _window, cx| {
            tracing::debug!(
                "unfocus handler: columns={}, indexes={}, constraints={}",
                unfocus_columns,
                unfocus_indexes,
                unfocus_constraints
            );
            if unfocus_columns {
                view.columns_table.update(cx, handler);
            }
            if unfocus_indexes {
                view.indexes_table.update(cx, handler);
            }
            if unfocus_constraints {
                view.constraints_table.update(cx, handler);
            }
        }
    }

    #[tracing::instrument(name = "schema_view_copy_focused_cell", skip_all)]
    fn copy_focused_cell(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!("got a copy request for the schema view");
        let Some(schema) = self.schema() else {
            tracing::debug!("copy request ignored: schema not ready");
            return;
        };
        let theme = cx.theme();

        let text = if let Some((row, col)) = self.columns_table.read(cx).focused_cell() {
            Self::columns_table_cell_value(schema, theme, row, col)
        } else if let Some((row, col)) = self.indexes_table.read(cx).focused_cell() {
            Self::indexes_table_cell_value(schema, theme, row, col)
        } else if let Some((row, col)) = self.constraints_table.read(cx).focused_cell() {
            Self::constraints_table_cell_value(schema, theme, row, col)
        } else {
            None
        };
        let Some(text) = text else {
            tracing::debug!("copy request ignored: no focused cell in any schema table");
            return;
        };

        tracing::debug!("copying focused schema cell to clipboard: {text}");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn columns_table_cell_value(
        schema: &RelationSchema,
        theme: &Theme,
        row: usize,
        col: usize,
    ) -> Option<String> {
        schema.columns.get(row).and_then(|column| {
            Self::columns_table_cells(column, schema, theme)
                .get(col)
                .map(|(_cell, value)| value.clone())
        })
    }

    fn indexes_table_cell_value(
        schema: &RelationSchema,
        theme: &Theme,
        row: usize,
        col: usize,
    ) -> Option<String> {
        schema.indexes.get(row).and_then(|index| {
            Self::indexes_table_cells(index, theme)
                .get(col)
                .map(|(_cell, value)| value.clone())
        })
    }

    fn constraints_table_cell_value(
        schema: &RelationSchema,
        theme: &Theme,
        row: usize,
        col: usize,
    ) -> Option<String> {
        schema.constraints.get(row).and_then(|constraint| {
            Self::constraints_table_cells(constraint, theme)
                .get(col)
                .map(|(_cell, value)| value.clone())
        })
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
            .id("schema-tab-view")
            .key_context(BINDING_CONTEXT)
            .on_action(cx.listener(Self::copy_focused_cell))
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .min_h_0()
            .flex_1()
            .bg(rgb(cx.theme().colors.bg_app))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use zsql_core::{ColumnDetail, ForeignKeyRef};

    use super::cells;
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
        assert_eq!(
            cells::rail_color(&column, &theme),
            Some(theme.colors.accent)
        );
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
        assert_eq!(
            cells::rail_color(&column, &theme),
            Some(theme.colors.key_fk)
        );
    }

    #[test]
    fn rail_color_is_none_for_a_plain_column() {
        assert_eq!(cells::rail_color(&plain_column(), &Theme::default()), None);
    }

    #[test]
    fn relation_kind_pill_text_maps_every_kind() {
        use zsql_core::RelationKind;
        assert_eq!(cells::relation_kind_pill_text(RelationKind::Table), "TABLE");
        assert_eq!(cells::relation_kind_pill_text(RelationKind::View), "VIEW");
        assert_eq!(
            cells::relation_kind_pill_text(RelationKind::MatView),
            "MATERIALIZED VIEW"
        );
        assert_eq!(
            cells::relation_kind_pill_text(RelationKind::Partitioned),
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
            _filters: &zsql_core::FilterState,
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
