//! Simple per-tab filter conditions for a generated preview: the v0 operator
//! set, the AND/OR connectors chaining them, and the best-effort value
//! quoting/expression classification every driver's `WHERE`-clause builder
//! shares. Pure and driver-agnostic, like [`crate::preview_state`] beside it
//! -- no dialect-specific identifier quoting or `ILIKE` mapping lives here;
//! that stays in each driver, which renders a [`FilterState`] via
//! [`render_where_conditions`] with its own quoting and dialect choices.

use std::fmt::Write as _;

/// The v0 filter operator set. No other operator is reachable from
/// [`FilterState`]'s API or the UI's operator menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOperator {
    Eq,
    Lt,
    Gt,
    Le,
    Ge,
    Like,
    ILike,
}

impl FilterOperator {
    /// Every v0 operator, in the order the operator menu lists them.
    pub const ALL: [FilterOperator; 7] = [
        FilterOperator::Eq,
        FilterOperator::Lt,
        FilterOperator::Gt,
        FilterOperator::Le,
        FilterOperator::Ge,
        FilterOperator::Like,
        FilterOperator::ILike,
    ];

    /// The operator's SQL symbol/keyword, as it reads on a chip and in
    /// generated SQL (before any dialect-specific `ILIKE` mapping).
    #[must_use]
    pub fn as_sql_symbol(self) -> &'static str {
        match self {
            FilterOperator::Eq => "=",
            FilterOperator::Lt => "<",
            FilterOperator::Gt => ">",
            FilterOperator::Le => "<=",
            FilterOperator::Ge => ">=",
            FilterOperator::Like => "LIKE",
            FilterOperator::ILike => "ILIKE",
        }
    }

    /// The operator menu's pattern hint, shown only for `LIKE`/`ILIKE`
    /// (`None` for every other operator).
    #[must_use]
    pub fn pattern_hint(self) -> Option<&'static str> {
        match self {
            FilterOperator::Like => Some("% and _"),
            FilterOperator::ILike => Some("ignores case"),
            _ => None,
        }
    }
}

/// The connector joining one filter chip to the next. Emitted into the
/// generated `WHERE` clause verbatim, in chip order, with no added
/// parentheses: precedence is SQL's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterConnector {
    And,
    Or,
}

impl FilterConnector {
    /// The connector's SQL keyword.
    #[must_use]
    pub fn as_sql(self) -> &'static str {
        match self {
            FilterConnector::And => "AND",
            FilterConnector::Or => "OR",
        }
    }

    /// The connector's flip: what a click on its pill applies.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            FilterConnector::And => FilterConnector::Or,
            FilterConnector::Or => FilterConnector::And,
        }
    }
}

/// How a filter value renders into generated SQL text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterValueRender {
    /// A quoted+escaped string, or a bare numeric literal, ready to embed
    /// as-is.
    Literal(String),
    /// An expression (function call, arithmetic, interval syntax), passed
    /// through unquoted verbatim. The chip that renders this marks it `fx`.
    Expression(String),
}

impl FilterValueRender {
    /// The exact text to embed in generated SQL.
    #[must_use]
    pub fn sql_text(&self) -> &str {
        match self {
            FilterValueRender::Literal(text) | FilterValueRender::Expression(text) => text,
        }
    }

    /// Whether this value passed through as an unquoted expression.
    #[must_use]
    pub fn is_expression(&self) -> bool {
        matches!(self, FilterValueRender::Expression(_))
    }
}

/// Backend type-name roots (matched whole, case-insensitively, after
/// stripping any parenthesized parameter list and `MySQL`'s trailing
/// `unsigned`/`zerofill` modifiers) that mark a column as numeric-like, so
/// its filter values stay bare rather than quoted. Covers each supported
/// driver family's own spellings: Postgres (`int2`/`int4`/`int8`/`numeric`/
/// `real`/`double precision`/`money`/`serial` family), `MySQL`
/// (`int`/`tinyint`/`decimal`/`float`/`double`), `MSSQL`
/// (`smallint`/`bigint`/`decimal`/`money`/`bit`), and `SQLite`'s dynamic
/// `INTEGER`/`REAL`/`NUMERIC` affinities. Matched as a whole root rather
/// than a substring, so a type that merely embeds one of these words --
/// `interval` embeds `int`, `point` embeds `int` -- is never misread as
/// numeric.
const NUMERIC_TYPE_ROOTS: &[&str] = &[
    "int2",
    "int4",
    "int8",
    "int",
    "integer",
    "smallint",
    "bigint",
    "tinyint",
    "mediumint",
    "numeric",
    "decimal",
    "dec",
    "real",
    "float",
    "float4",
    "float8",
    "double",
    "double precision",
    "money",
    "smallmoney",
    "bit",
    "serial",
    "bigserial",
    "smallserial",
];

/// Reduce `type_name` to the bare root [`NUMERIC_TYPE_ROOTS`] matches
/// against: lowercased, trimmed, with any trailing `(...)` parameter list
/// (`decimal(10,2)` -> `decimal`) and `MySQL`'s trailing `unsigned`/
/// `zerofill` modifiers dropped.
fn numeric_type_root(type_name: &str) -> String {
    let lower = type_name.trim().to_ascii_lowercase();
    let base = match lower.find('(') {
        Some(paren) => lower[..paren].trim_end(),
        None => lower.as_str(),
    };
    base.trim_end_matches(" zerofill")
        .trim_end_matches(" unsigned")
        .trim()
        .to_owned()
}

/// Whether `type_name` (a [`crate::value::ColumnMeta::type_name`]) reads as
/// a numeric backend type, so a filter value against it stays bare rather
/// than quoted. Best-effort: `type_name` is reduced to its bare root (see
/// [`numeric_type_root`]) and checked against [`NUMERIC_TYPE_ROOTS`] as a
/// whole word, not a substring, so any driver's own casing and width
/// suffixes (`int4`, `BIGINT`, `Decimal(10,2)`) still match while a type
/// that merely embeds a numeric-sounding word (`interval`, `point`) does
/// not. Everything else -- text, character, uuid, json, timestamp/date, and
/// any unrecognized type -- is treated as text-like and quoted, since a
/// wrongly quoted numeric merely fails at the database (a syntax the driver
/// rejects), while a wrongly bare string silently breaks the query.
#[must_use]
fn type_is_numeric_like(type_name: &str) -> bool {
    let root = numeric_type_root(type_name);
    NUMERIC_TYPE_ROOTS.contains(&root.as_str())
}

/// Whether `trimmed` parses as a plain signed/unsigned integer or
/// floating-point literal -- a numeric value, not an arithmetic expression,
/// even though a leading `-` looks operator-shaped.
fn is_plain_number(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.parse::<f64>().is_ok()
}

/// Whether `trimmed` reads as a SQL expression rather than a plain value:
/// a function call (contains `(`), `interval` syntax, or an arithmetic
/// combination of terms. A bare number (including a negative one) never
/// counts, so `-5` classifies as a numeric literal rather than "negation".
/// A combination of terms is only recognized when an arithmetic operator
/// stands as its own whitespace-separated token (`total_cents + 500`), so a
/// plain literal that merely contains `+`/`-`/`*`/`/` -- a date
/// (`2026-07-27`), a UUID (`550e8400-e29b-41d4-a716-446655440000`), or
/// hyphenated text (`well-known`) -- is never misread as arithmetic.
fn looks_like_expression(trimmed: &str) -> bool {
    if trimmed.is_empty() || is_plain_number(trimmed) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.contains('(') || lower.starts_with("interval ") {
        return true;
    }
    trimmed
        .split_whitespace()
        .any(|token| matches!(token, "+" | "-" | "*" | "/"))
}

/// Single-quote `value` for embedding as a SQL string literal, doubling any
/// embedded single quote (`it's` -> `'it''s'`) so it can never break out of
/// its quotes. Deliberately separate from [`crate::sql::quote_ident`]: this
/// quotes a *value*, not an identifier, and the two must never be conflated.
#[must_use]
pub fn quote_sql_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// Classify and render `raw` for a column whose backend type is
/// `type_name`: a value that parses as an expression (function call,
/// arithmetic, interval syntax) passes through unquoted and is marked `fx`;
/// everything else is a plain value, quoted+escaped for a text-like column
/// or left bare for a numeric-like one (see [`type_is_numeric_like`]). An
/// empty value is always quoted, even against a numeric-like column, so a
/// filter left blank still renders as valid (if type-mismatched) SQL rather
/// than a bare, syntactically invalid right-hand side.
#[must_use]
pub fn classify_filter_value(raw: &str, type_name: &str) -> FilterValueRender {
    let trimmed = raw.trim();
    if looks_like_expression(trimmed) {
        return FilterValueRender::Expression(trimmed.to_owned());
    }
    if !trimmed.is_empty() && type_is_numeric_like(type_name) {
        FilterValueRender::Literal(trimmed.to_owned())
    } else {
        FilterValueRender::Literal(quote_sql_string(trimmed))
    }
}

/// Stable identity for one [`FilterState`] condition, unique within that
/// state for its lifetime.
pub type FilterConditionId = u64;

/// One committed filter chip: the column it targets, that column's backend
/// type (captured at add/edit time so rendering never needs a fresh column
/// lookup), the v0 operator, and the value as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterCondition {
    id: FilterConditionId,
    column: String,
    type_name: String,
    operator: FilterOperator,
    value: String,
}

impl FilterCondition {
    #[must_use]
    pub fn id(&self) -> FilterConditionId {
        self.id
    }

    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    #[must_use]
    pub fn operator(&self) -> FilterOperator {
        self.operator
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// This condition's value, classified and rendered per
    /// [`classify_filter_value`] against [`FilterCondition::type_name`].
    #[must_use]
    pub fn rendered_value(&self) -> FilterValueRender {
        classify_filter_value(&self.value, &self.type_name)
    }
}

/// An ordered set of committed filter conditions plus the AND/OR connectors
/// between adjacent ones: `connectors[i]` joins `conditions[i]` to
/// `conditions[i + 1]`, so `connectors.len() == conditions.len() - 1`
/// whenever `conditions` is non-empty (`0` when it holds 0 or 1 condition).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterState {
    conditions: Vec<FilterCondition>,
    connectors: Vec<FilterConnector>,
    next_id: FilterConditionId,
}

impl FilterState {
    /// An empty filter state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The committed conditions, in chip order.
    #[must_use]
    pub fn conditions(&self) -> &[FilterCondition] {
        &self.conditions
    }

    /// The connectors between adjacent conditions: `connectors()[i]` joins
    /// `conditions()[i]` to `conditions()[i + 1]`.
    #[must_use]
    pub fn connectors(&self) -> &[FilterConnector] {
        &self.connectors
    }

    /// Whether no filter is currently committed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }

    /// How many filters are currently committed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.conditions.len()
    }

    /// Whether `column` currently carries at least one active filter, for
    /// the grid header's funnel marker.
    #[must_use]
    pub fn column_is_filtered(&self, column: &str) -> bool {
        self.conditions.iter().any(|c| c.column == column)
    }

    /// Append a new committed condition, joined to the previous one (if any)
    /// with `AND`, and return its id.
    pub fn add_condition(
        &mut self,
        column: impl Into<String>,
        type_name: impl Into<String>,
        operator: FilterOperator,
        value: impl Into<String>,
    ) -> FilterConditionId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if !self.conditions.is_empty() {
            self.connectors.push(FilterConnector::And);
        }
        self.conditions.push(FilterCondition {
            id,
            column: column.into(),
            type_name: type_name.into(),
            operator,
            value: value.into(),
        });
        id
    }

    /// Remove the condition with `id`, and whichever connector joined it to
    /// its remaining neighbor. Returns whether a condition was actually
    /// removed.
    pub fn remove_condition(&mut self, id: FilterConditionId) -> bool {
        let Some(index) = self.conditions.iter().position(|c| c.id == id) else {
            return false;
        };
        self.conditions.remove(index);
        // `connectors[index]` joined the removed condition to the one after
        // it, if there was one; otherwise `connectors[index - 1]` joined it
        // to the one before it.
        if index < self.connectors.len() {
            self.connectors.remove(index);
        } else if index > 0 {
            self.connectors.remove(index - 1);
        }
        true
    }

    /// Replace the operator/value of the condition with `id`, keeping its
    /// column and type unchanged. Returns whether a matching condition was
    /// found.
    pub fn update_condition(
        &mut self,
        id: FilterConditionId,
        operator: FilterOperator,
        value: impl Into<String>,
    ) -> bool {
        let Some(condition) = self.conditions.iter_mut().find(|c| c.id == id) else {
            return false;
        };
        condition.operator = operator;
        condition.value = value.into();
        true
    }

    /// Toggle the connector at `index` (joining `conditions()[index]` to
    /// `conditions()[index + 1]`) between `AND` and `OR`. Returns whether
    /// `index` was in bounds.
    pub fn toggle_connector(&mut self, index: usize) -> bool {
        let Some(connector) = self.connectors.get_mut(index) else {
            return false;
        };
        *connector = connector.toggled();
        true
    }

    /// Remove every condition and connector in one action. Returns whether
    /// anything was actually cleared.
    pub fn clear(&mut self) -> bool {
        if self.conditions.is_empty() {
            return false;
        }
        self.conditions.clear();
        self.connectors.clear();
        true
    }
}

/// Render `state`'s conditions/connectors as a `WHERE` clause's contents
/// (without the leading `WHERE` keyword), quoting each condition's column
/// via `quote_column` -- always a driver's own identifier-quoting helper,
/// never free-text interpolation -- and mapping `ILIKE` per `ilike_native`:
/// a native `ILIKE` operator when `true` (Postgres), or
/// `LOWER(column) LIKE LOWER(value)` when `false` (every other supported
/// dialect). `None` when `state` holds no conditions.
#[must_use]
pub fn render_where_conditions(
    state: &FilterState,
    quote_column: impl Fn(&str) -> String,
    ilike_native: bool,
) -> Option<String> {
    if state.conditions.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (index, condition) in state.conditions.iter().enumerate() {
        if index > 0 {
            let connector = state
                .connectors
                .get(index - 1)
                .copied()
                .unwrap_or(FilterConnector::And);
            let _ = write!(out, " {} ", connector.as_sql());
        }
        let column_sql = quote_column(condition.column());
        let value_text = condition.rendered_value().sql_text().to_owned();
        if condition.operator() == FilterOperator::ILike && !ilike_native {
            let _ = write!(out, "LOWER({column_sql}) LIKE LOWER({value_text})");
        } else {
            let _ = write!(
                out,
                "{column_sql} {} {value_text}",
                condition.operator().as_sql_symbol()
            );
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{
        FilterConnector, FilterOperator, FilterState, FilterValueRender, classify_filter_value,
        quote_sql_string, render_where_conditions,
    };

    // -- quote_sql_string ---------------------------------------------------

    #[test]
    fn quote_sql_string_wraps_a_plain_value_in_single_quotes() {
        assert_eq!(quote_sql_string("paid"), "'paid'");
    }

    #[test]
    fn quote_sql_string_doubles_an_embedded_single_quote() {
        assert_eq!(quote_sql_string("it's"), "'it''s'");
    }

    #[test]
    fn quote_sql_string_doubles_every_embedded_quote() {
        assert_eq!(quote_sql_string("''"), "''''''");
    }

    #[test]
    fn quote_sql_string_is_safe_against_an_injection_attempting_value() {
        let quoted = quote_sql_string("x'; DROP TABLE users; --");
        assert_eq!(quoted, "'x''; DROP TABLE users; --'");
        assert_eq!(quoted.matches("DROP TABLE").count(), 1);
    }

    // -- classify_filter_value: representative type_name strings ------------

    #[test]
    fn classify_quotes_a_postgres_text_column() {
        assert_eq!(
            classify_filter_value("paid", "text"),
            FilterValueRender::Literal("'paid'".to_owned())
        );
    }

    #[test]
    fn classify_leaves_a_postgres_int_family_column_bare() {
        for type_name in ["int2", "int4", "int8", "numeric", "real", "money"] {
            assert_eq!(
                classify_filter_value("8500", type_name),
                FilterValueRender::Literal("8500".to_owned()),
                "type_name={type_name}"
            );
        }
    }

    #[test]
    fn classify_leaves_a_mysql_numeric_column_bare() {
        for type_name in [
            "int",
            "bigint",
            "tinyint",
            "decimal",
            "float",
            "double",
            "int unsigned",
            "int(10) unsigned zerofill",
            "double precision",
        ] {
            assert_eq!(
                classify_filter_value("12", type_name),
                FilterValueRender::Literal("12".to_owned()),
                "type_name={type_name}"
            );
        }
    }

    #[test]
    fn classify_quotes_a_mysql_text_column() {
        for type_name in ["varchar(255)", "text", "char(10)", "json", "datetime"] {
            assert_eq!(
                classify_filter_value("hi", type_name),
                FilterValueRender::Literal("'hi'".to_owned()),
                "type_name={type_name}"
            );
        }
    }

    #[test]
    fn classify_leaves_a_mssql_numeric_column_bare() {
        for type_name in ["smallint", "bigint", "decimal(10,2)", "money", "bit"] {
            assert_eq!(
                classify_filter_value("1", type_name),
                FilterValueRender::Literal("1".to_owned()),
                "type_name={type_name}"
            );
        }
    }

    #[test]
    fn classify_quotes_a_mssql_text_column() {
        for type_name in ["nvarchar", "varchar", "uniqueidentifier", "datetime2"] {
            assert_eq!(
                classify_filter_value("x", type_name),
                FilterValueRender::Literal("'x'".to_owned()),
                "type_name={type_name}"
            );
        }
    }

    #[test]
    fn classify_leaves_a_sqlite_numeric_affinity_bare() {
        for type_name in ["INTEGER", "REAL", "NUMERIC"] {
            assert_eq!(
                classify_filter_value("3", type_name),
                FilterValueRender::Literal("3".to_owned()),
                "type_name={type_name}"
            );
        }
    }

    #[test]
    fn classify_quotes_a_sqlite_text_affinity() {
        assert_eq!(
            classify_filter_value("hi", "TEXT"),
            FilterValueRender::Literal("'hi'".to_owned())
        );
    }

    #[test]
    fn classify_escapes_an_embedded_quote_in_a_text_value() {
        assert_eq!(
            classify_filter_value("it's", "text"),
            FilterValueRender::Literal("'it''s'".to_owned())
        );
    }

    #[test]
    fn classify_quotes_a_plain_value_against_an_interval_typed_column() {
        // "interval" embeds the substring "int" but is not numeric-like: a
        // plain value against it must still be quoted, not left bare.
        assert_eq!(
            classify_filter_value("7", "interval"),
            FilterValueRender::Literal("'7'".to_owned())
        );
    }

    #[test]
    fn classify_quotes_a_plain_value_against_a_point_typed_column() {
        // "point" embeds the substring "int" too; same guard applies.
        assert_eq!(
            classify_filter_value("5", "point"),
            FilterValueRender::Literal("'5'".to_owned())
        );
    }

    #[test]
    fn classify_quotes_a_numeric_looking_value_for_an_unrecognized_type() {
        // A default/unrecognized type_name is treated as text-like, so a
        // value that is itself numeric text is still quoted like any other
        // plain value -- classification is driven by the column's declared
        // type, not by sniffing the value's own shape.
        assert_eq!(
            classify_filter_value("8500", "geometry"),
            FilterValueRender::Literal("'8500'".to_owned())
        );
    }

    // -- classify_filter_value: expression pass-through ----------------------

    #[test]
    fn classify_passes_a_function_call_through_unquoted() {
        assert_eq!(
            classify_filter_value("now()", "timestamptz"),
            FilterValueRender::Expression("now()".to_owned())
        );
    }

    #[test]
    fn classify_passes_a_function_call_with_arithmetic_through_unquoted() {
        assert_eq!(
            classify_filter_value("now() - interval '7 days'", "timestamptz"),
            FilterValueRender::Expression("now() - interval '7 days'".to_owned())
        );
    }

    #[test]
    fn classify_passes_interval_syntax_through_unquoted() {
        assert_eq!(
            classify_filter_value("interval '7 days'", "interval"),
            FilterValueRender::Expression("interval '7 days'".to_owned())
        );
    }

    #[test]
    fn classify_passes_column_arithmetic_through_unquoted() {
        assert_eq!(
            classify_filter_value("total_cents + 500", "int4"),
            FilterValueRender::Expression("total_cents + 500".to_owned())
        );
    }

    #[test]
    fn classify_passes_division_through_unquoted() {
        assert_eq!(
            classify_filter_value("total_cents / 2", "int4"),
            FilterValueRender::Expression("total_cents / 2".to_owned())
        );
    }

    #[test]
    fn classify_passes_multiplication_through_unquoted() {
        assert_eq!(
            classify_filter_value("unit_price * qty", "numeric"),
            FilterValueRender::Expression("unit_price * qty".to_owned())
        );
    }

    #[test]
    fn classify_treats_a_negative_number_as_a_bare_literal_not_an_expression() {
        assert_eq!(
            classify_filter_value("-5", "int4"),
            FilterValueRender::Literal("-5".to_owned())
        );
    }

    #[test]
    fn classify_treats_a_positive_signed_number_as_a_bare_literal() {
        assert_eq!(
            classify_filter_value("+5", "int4"),
            FilterValueRender::Literal("+5".to_owned())
        );
    }

    #[test]
    fn classify_treats_a_float_literal_as_bare_not_an_expression() {
        assert_eq!(
            classify_filter_value("-3.5", "real"),
            FilterValueRender::Literal("-3.5".to_owned())
        );
    }

    #[test]
    fn classify_quotes_a_plain_date_value_rather_than_treating_it_as_arithmetic() {
        assert_eq!(
            classify_filter_value("2026-07-27", "date"),
            FilterValueRender::Literal("'2026-07-27'".to_owned())
        );
    }

    #[test]
    fn classify_quotes_a_uuid_value_rather_than_treating_it_as_arithmetic() {
        assert_eq!(
            classify_filter_value("550e8400-e29b-41d4-a716-446655440000", "uuid"),
            FilterValueRender::Literal("'550e8400-e29b-41d4-a716-446655440000'".to_owned())
        );
    }

    #[test]
    fn classify_quotes_hyphenated_text_rather_than_treating_it_as_arithmetic() {
        assert_eq!(
            classify_filter_value("well-known", "text"),
            FilterValueRender::Literal("'well-known'".to_owned())
        );
    }

    #[test]
    fn classify_does_not_panic_on_a_value_starting_with_a_multi_byte_character() {
        assert_eq!(
            classify_filter_value("\u{dc}ber", "text"),
            FilterValueRender::Literal("'\u{dc}ber'".to_owned())
        );
        assert_eq!(
            classify_filter_value("\u{f1}o\u{f1}o", "text"),
            FilterValueRender::Literal("'\u{f1}o\u{f1}o'".to_owned())
        );
        assert_eq!(
            classify_filter_value("\u{20ac}100", "text"),
            FilterValueRender::Literal("'\u{20ac}100'".to_owned())
        );
    }

    #[test]
    fn classify_quotes_an_empty_value_even_for_a_numeric_column() {
        assert_eq!(
            classify_filter_value("", "int4"),
            FilterValueRender::Literal("''".to_owned())
        );
        assert_eq!(
            classify_filter_value("   ", "int4"),
            FilterValueRender::Literal("''".to_owned())
        );
    }

    #[test]
    fn classify_trims_surrounding_whitespace() {
        assert_eq!(
            classify_filter_value("  paid  ", "text"),
            FilterValueRender::Literal("'paid'".to_owned())
        );
    }

    #[test]
    fn rendered_value_reports_is_expression() {
        let render = classify_filter_value("now()", "timestamptz");
        assert!(render.is_expression());
        let literal = classify_filter_value("paid", "text");
        assert!(!literal.is_expression());
    }

    // -- FilterState: add/remove/toggle/clear --------------------------------

    #[test]
    fn a_fresh_state_is_empty() {
        let state = FilterState::new();
        assert!(state.is_empty());
        assert_eq!(state.len(), 0);
        assert!(state.conditions().is_empty());
        assert!(state.connectors().is_empty());
    }

    #[test]
    fn add_condition_appends_and_returns_a_unique_id() {
        let mut state = FilterState::new();
        let first = state.add_condition("status", "text", FilterOperator::Eq, "paid");
        let second = state.add_condition("status", "text", FilterOperator::Eq, "pending");
        assert_ne!(first, second);
        assert_eq!(state.len(), 2);
        assert_eq!(state.conditions()[0].id(), first);
        assert_eq!(state.conditions()[1].id(), second);
    }

    #[test]
    fn add_condition_after_the_first_joins_with_and_by_default() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        state.add_condition("status", "text", FilterOperator::Eq, "pending");
        assert_eq!(state.connectors(), [FilterConnector::And]);
    }

    #[test]
    fn remove_condition_by_id_drops_it_and_its_joining_connector() {
        let mut state = FilterState::new();
        let a = state.add_condition("status", "text", FilterOperator::Eq, "paid");
        let b = state.add_condition("status", "text", FilterOperator::Eq, "pending");
        let c = state.add_condition("placed_at", "timestamptz", FilterOperator::Gt, "now()");
        assert!(state.remove_condition(b));
        assert_eq!(state.len(), 2);
        assert_eq!(
            state
                .conditions()
                .iter()
                .map(super::FilterCondition::id)
                .collect::<Vec<_>>(),
            [a, c]
        );
        assert_eq!(state.connectors().len(), 1);
    }

    #[test]
    fn remove_condition_of_the_last_chip_drops_the_preceding_connector() {
        let mut state = FilterState::new();
        state.add_condition("a", "text", FilterOperator::Eq, "1");
        let last = state.add_condition("b", "text", FilterOperator::Eq, "2");
        assert!(state.remove_condition(last));
        assert!(state.connectors().is_empty());
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn remove_condition_returns_false_for_an_unknown_id() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        assert!(!state.remove_condition(9999));
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn remove_condition_of_the_only_condition_clears_the_state() {
        let mut state = FilterState::new();
        let id = state.add_condition("status", "text", FilterOperator::Eq, "paid");
        assert!(state.remove_condition(id));
        assert!(state.is_empty());
    }

    #[test]
    fn toggle_connector_flips_and_to_or_and_back() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        state.add_condition("status", "text", FilterOperator::Eq, "pending");
        assert!(state.toggle_connector(0));
        assert_eq!(state.connectors(), [FilterConnector::Or]);
        assert!(state.toggle_connector(0));
        assert_eq!(state.connectors(), [FilterConnector::And]);
    }

    #[test]
    fn toggle_connector_returns_false_out_of_bounds() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        assert!(!state.toggle_connector(0));
    }

    #[test]
    fn update_condition_replaces_operator_and_value_but_keeps_column_and_type() {
        let mut state = FilterState::new();
        let id = state.add_condition("total_cents", "int4", FilterOperator::Eq, "100");
        assert!(state.update_condition(id, FilterOperator::Ge, "500"));
        let condition = &state.conditions()[0];
        assert_eq!(condition.column(), "total_cents");
        assert_eq!(condition.type_name(), "int4");
        assert_eq!(condition.operator(), FilterOperator::Ge);
        assert_eq!(condition.value(), "500");
    }

    #[test]
    fn update_condition_returns_false_for_an_unknown_id() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        assert!(!state.update_condition(9999, FilterOperator::Eq, "x"));
    }

    #[test]
    fn clear_removes_every_condition_and_connector() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        state.add_condition("status", "text", FilterOperator::Eq, "pending");
        assert!(state.clear());
        assert!(state.is_empty());
        assert!(state.connectors().is_empty());
    }

    #[test]
    fn clear_on_an_already_empty_state_returns_false() {
        let mut state = FilterState::new();
        assert!(!state.clear());
    }

    #[test]
    fn column_is_filtered_reflects_any_active_condition_on_that_column() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        assert!(state.column_is_filtered("status"));
        assert!(!state.column_is_filtered("placed_at"));
    }

    // -- render_where_conditions ----------------------------------------------

    #[test]
    fn render_where_conditions_is_none_for_an_empty_state() {
        let state = FilterState::new();
        assert_eq!(
            render_where_conditions(&state, |c| format!("\"{c}\""), true),
            None
        );
    }

    #[test]
    fn render_where_conditions_renders_a_single_condition() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        assert_eq!(
            render_where_conditions(&state, |c| format!("\"{c}\""), true),
            Some("\"status\" = 'paid'".to_owned())
        );
    }

    #[test]
    fn render_where_conditions_joins_multiple_with_their_own_connectors() {
        let mut state = FilterState::new();
        state.add_condition("status", "text", FilterOperator::Eq, "paid");
        state.add_condition("status", "text", FilterOperator::Eq, "pending");
        state.toggle_connector(0);
        state.add_condition("placed_at", "timestamptz", FilterOperator::Gt, "now()");
        assert_eq!(
            render_where_conditions(&state, |c| format!("\"{c}\""), true),
            Some(
                "\"status\" = 'paid' OR \"status\" = 'pending' AND \"placed_at\" > now()"
                    .to_owned()
            )
        );
    }

    #[test]
    fn render_where_conditions_uses_native_ilike_when_requested() {
        let mut state = FilterState::new();
        state.add_condition("name", "text", FilterOperator::ILike, "smith%");
        assert_eq!(
            render_where_conditions(&state, |c| format!("\"{c}\""), true),
            Some("\"name\" ILIKE 'smith%'".to_owned())
        );
    }

    #[test]
    fn render_where_conditions_maps_ilike_to_lower_like_lower_when_not_native() {
        let mut state = FilterState::new();
        state.add_condition("name", "text", FilterOperator::ILike, "smith%");
        assert_eq!(
            render_where_conditions(&state, |c| format!("`{c}`"), false),
            Some("LOWER(`name`) LIKE LOWER('smith%')".to_owned())
        );
    }

    #[test]
    fn render_where_conditions_renders_each_comparison_operator_symbol() {
        for (operator, symbol) in [
            (FilterOperator::Lt, "<"),
            (FilterOperator::Le, "<="),
            (FilterOperator::Ge, ">="),
        ] {
            let mut state = FilterState::new();
            state.add_condition("total_cents", "int4", operator, "5000");
            assert_eq!(
                render_where_conditions(&state, |c| format!("\"{c}\""), true),
                Some(format!("\"total_cents\" {symbol} 5000")),
                "operator {operator:?} must emit its SQL symbol verbatim"
            );
        }
    }

    #[test]
    fn render_where_conditions_renders_a_case_sensitive_like_pattern() {
        let mut state = FilterState::new();
        state.add_condition("name", "text", FilterOperator::Like, "Smith%");
        assert_eq!(
            render_where_conditions(&state, |c| format!("\"{c}\""), true),
            Some("\"name\" LIKE 'Smith%'".to_owned())
        );
        assert_eq!(
            render_where_conditions(&state, |c| format!("`{c}`"), false),
            Some("`name` LIKE 'Smith%'".to_owned()),
            "plain LIKE is case-sensitive everywhere, so no LOWER() wrapping on any dialect"
        );
    }

    #[test]
    fn pattern_hint_annotates_only_the_like_operators() {
        assert_eq!(FilterOperator::Like.pattern_hint(), Some("% and _"));
        assert_eq!(FilterOperator::ILike.pattern_hint(), Some("ignores case"));
        for operator in [
            FilterOperator::Eq,
            FilterOperator::Lt,
            FilterOperator::Gt,
            FilterOperator::Le,
            FilterOperator::Ge,
        ] {
            assert_eq!(
                operator.pattern_hint(),
                None,
                "comparison operator {operator:?} carries no pattern hint"
            );
        }
    }

    #[test]
    fn render_where_conditions_quotes_the_column_via_the_given_quoting_fn() {
        let mut state = FilterState::new();
        state.add_condition("weird\"col", "text", FilterOperator::Eq, "x");
        let rendered = render_where_conditions(&state, crate::sql::quote_ident, true);
        assert_eq!(rendered, Some("\"weird\"\"col\" = 'x'".to_owned()));
    }
}
