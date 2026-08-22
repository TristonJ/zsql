//! Driver-aware parameter detection, heuristic type inference, and safe
//! client-side substitution over raw SQL text, independent of any UI.
//!
//! `:name` is detected on every driver. `@name` (T-SQL) is detected only
//! for the `mssql` driver. A bare `?` (positional) is detected only for
//! the `mysql` and `sqlite` drivers.
//!
//! Detection recognizes single- and double-quoted regions but not a
//! dialect's own backtick- or bracket-quoted identifiers, so a gated token
//! inside one (a `?` in a MySQL `` `col?` ``, an `@name` in a T-SQL
//! `[col@name]`) is still read as a parameter.

use std::collections::{HashMap, HashSet};

use crate::filter::quote_sql_string;

/// The lexical form of a detected parameter token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamKind {
    /// `:name`, detected on every driver.
    Colon,
    /// `@name`, detected only on the `mssql` driver.
    At,
    /// A bare `?`, detected only on the `mysql` and `sqlite` drivers.
    Positional,
}

/// One parameter token found in SQL text: its lexical form, the identifier
/// it names (empty for a [`ParamKind::Positional`] `?`, which has none),
/// the byte offset of its leading symbol, its length in bytes (a `?` token
/// includes any trailing digit run), and the 1-based line it appears on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamOccurrence {
    pub name: String,
    pub kind: ParamKind,
    pub offset: usize,
    pub token_len: usize,
    pub line: usize,
}

/// A heuristically inferred display type for a parameter. Advisory only:
/// it never changes how a value is escaped or validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Date,
    Numeric,
    Integer,
    Text,
}

impl ParamType {
    /// The uppercase badge label this type renders as.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ParamType::Date => "DATE",
            ParamType::Numeric => "NUMERIC",
            ParamType::Integer => "INTEGER",
            ParamType::Text => "TEXT",
        }
    }
}

/// One logical parameter: every occurrence in the SQL text sharing its kind
/// and name, though a `?` never shares: each is its own single-occurrence
/// parameter, named `?1`, `?2`, ... in occurrence order. Also carries its
/// heuristically inferred type (from the first occurrence's surrounding
/// expression).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub kind: ParamKind,
    pub occurrences: Vec<ParamOccurrence>,
    pub inferred_type: ParamType,
}

impl Parameter {
    /// The identifier a value or remembered history entry for this
    /// parameter is keyed by: `name` unchanged for [`ParamKind::Colon`] and
    /// [`ParamKind::Positional`] (already unique, and stable for a
    /// `:name`'s existing remembered history), `@`-prefixed for
    /// [`ParamKind::At`] so it can never collide with a `:name` of the same
    /// identifier.
    #[must_use]
    pub fn storage_key(&self) -> String {
        match self.kind {
            ParamKind::At => format!("@{}", self.name),
            ParamKind::Colon | ParamKind::Positional => self.name.clone(),
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The driver id ([`crate::driver::Driver::id`]) whose native parameter
/// syntax is `@name`: the only driver this module also excludes
/// `@@name` system variables and `DECLARE`d T-SQL variables for.
const MSSQL_DRIVER_ID: &str = "mssql";

/// The driver id ([`crate::driver::Driver::id`]) of the one supported
/// backend whose string literals treat a backslash as an escape character
/// by default, rather than a literal character: MySQL/MariaDB. Every other
/// supported backend uses standard-conforming string literals, where a
/// backslash carries no special meaning.
const MYSQL_DRIVER_ID: &str = "mysql";

/// The driver id ([`crate::driver::Driver::id`]) of the other driver whose
/// native parameter syntax is a positional `?`, alongside
/// [`MYSQL_DRIVER_ID`].
const SQLITE_DRIVER_ID: &str = "sqlite";

/// Whether `driver_id` detects `@name` as a native parameter token.
fn allows_at_params(driver_id: &str) -> bool {
    driver_id == MSSQL_DRIVER_ID
}

/// Whether `driver_id` detects a bare `?` as a native positional parameter
/// token.
fn allows_positional_params(driver_id: &str) -> bool {
    driver_id == MYSQL_DRIVER_ID || driver_id == SQLITE_DRIVER_ID
}

/// The lexical region the scan is currently inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Normal,
    SingleQuoted,
    DoubleQuoted,
    LineComment,
    BlockComment,
}

/// Advance past one byte of a quoted region delimited by `quote` (a single
/// or double quote), where a doubled `quote` is an escaped quote inside the
/// region, not its close. Returns the next index and the region's state:
/// still `quote_state` while open, [`ScanState::Normal`] once closed.
fn advance_quoted(
    bytes: &[u8],
    i: usize,
    len: usize,
    quote: u8,
    quote_state: ScanState,
) -> (usize, ScanState) {
    if bytes[i] != quote {
        return (i + 1, quote_state);
    }
    if i + 1 < len && bytes[i + 1] == quote {
        return (i + 2, quote_state);
    }
    (i + 1, ScanState::Normal)
}

/// Advance past one byte of a `/* */` block comment, closing it on `*/`.
fn advance_block_comment(bytes: &[u8], i: usize, len: usize) -> (usize, ScanState) {
    if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
        (i + 2, ScanState::Normal)
    } else {
        (i + 1, ScanState::BlockComment)
    }
}

/// Advance past one byte of a `--` line comment, closing it after `\n`.
fn advance_line_comment(bytes: &[u8], i: usize) -> (usize, ScanState) {
    if bytes[i] == b'\n' {
        (i + 1, ScanState::Normal)
    } else {
        (i + 1, ScanState::LineComment)
    }
}

/// Progress through a possible `DECLARE @a type, @b type, ...;` list while
/// scanning `sql` for the T-SQL variable names it declares.
struct DeclareListScan {
    declared: HashSet<String>,
    awaiting_declared_name: bool,
    in_declare_list: bool,
    paren_depth: i32,
    // Ordinary identifiers seen since the last declared name (or
    // list-separating comma) while still inside a top-level DECLARE list: a
    // variable's type is the first one and keeps the list open, a second
    // one in a row means the batch has moved on to its next statement
    // without a closing ';'.
    plain_idents_since_name: u32,
}

impl DeclareListScan {
    fn new() -> Self {
        Self {
            declared: HashSet::new(),
            awaiting_declared_name: false,
            in_declare_list: false,
            paren_depth: 0,
            plain_idents_since_name: 0,
        }
    }

    fn open_declare_list(&mut self) {
        self.awaiting_declared_name = true;
        self.in_declare_list = true;
        self.paren_depth = 0;
        self.plain_idents_since_name = 0;
    }

    fn close_declare_list(&mut self) {
        self.in_declare_list = false;
        self.awaiting_declared_name = false;
        self.paren_depth = 0;
        self.plain_idents_since_name = 0;
    }

    fn separate_by_comma(&mut self) {
        self.awaiting_declared_name = true;
        self.plain_idents_since_name = 0;
    }

    fn record_declared_name(&mut self, name: &str) {
        self.declared.insert(name.to_ascii_lowercase());
        self.awaiting_declared_name = false;
        self.plain_idents_since_name = 0;
    }

    /// Registers one ordinary (non-`@name`) identifier; a second one in a
    /// row past a variable's own type closes the list without a `;`.
    fn record_plain_ident(&mut self) {
        self.awaiting_declared_name = false;
        if self.in_declare_list && self.paren_depth == 0 {
            self.plain_idents_since_name += 1;
            if self.plain_idents_since_name > 1 {
                self.close_declare_list();
            }
        }
    }
}

/// Advance past one byte outside any quoted region or comment while
/// scanning for `DECLARE`d T-SQL variable names, updating `scan` as it
/// recognizes `@@name`, an awaited `@name`, parens, `;`, a list-separating
/// `,`, and the `DECLARE` keyword itself.
fn advance_declare_scan(
    sql: &str,
    bytes: &[u8],
    i: usize,
    len: usize,
    scan: &mut DeclareListScan,
) -> (usize, ScanState) {
    let b = bytes[i];
    if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
        return (i + 2, ScanState::LineComment);
    }
    if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
        return (i + 2, ScanState::BlockComment);
    }
    if b == b'\'' {
        return (i + 1, ScanState::SingleQuoted);
    }
    if b == b'"' {
        return (i + 1, ScanState::DoubleQuoted);
    }
    if b == b'@' && i + 1 < len && bytes[i + 1] == b'@' {
        // @@name is a system variable, never a declared variable's own
        // token.
        return (i + 2, ScanState::Normal);
    }
    if scan.awaiting_declared_name && b == b'@' && i + 1 < len && is_ident_start(bytes[i + 1]) {
        let mut j = i + 1;
        while j < len && is_ident_continue(bytes[j]) {
            j += 1;
        }
        scan.record_declared_name(&sql[i + 1..j]);
        return (j, ScanState::Normal);
    }
    if b == b'(' {
        scan.paren_depth += 1;
        return (i + 1, ScanState::Normal);
    }
    if b == b')' {
        scan.paren_depth -= 1;
        return (i + 1, ScanState::Normal);
    }
    if b == b';' {
        scan.close_declare_list();
        return (i + 1, ScanState::Normal);
    }
    if scan.in_declare_list && scan.paren_depth == 0 && b == b',' {
        scan.separate_by_comma();
        return (i + 1, ScanState::Normal);
    }
    if is_ident_start(b) {
        let mut j = i + 1;
        while j < len && is_ident_continue(bytes[j]) {
            j += 1;
        }
        if sql[i..j].eq_ignore_ascii_case("declare") {
            scan.open_declare_list();
        } else {
            scan.record_plain_ident();
        }
        return (j, ScanState::Normal);
    }
    (i + 1, ScanState::Normal)
}

/// Every T-SQL variable name `sql` `DECLARE`s, found lexically and matched
/// case-insensitively: the `@name` directly after each `DECLARE` keyword,
/// and after every later top-level comma in the same comma-separated
/// variable list (so `DECLARE @a INT, @b INT` excludes both). Not a SQL
/// parser: one ordinary identifier (a variable's type) between names
/// leaves the pending list open, but a second one in a row ends it.
fn collect_declared_at_names(sql: &str) -> HashSet<String> {
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut scan = DeclareListScan::new();
    let mut state = ScanState::Normal;
    let mut i = 0usize;

    while i < len {
        let (next_i, next_state) = match state {
            ScanState::Normal => advance_declare_scan(sql, bytes, i, len, &mut scan),
            ScanState::SingleQuoted => advance_quoted(bytes, i, len, b'\'', state),
            ScanState::DoubleQuoted => advance_quoted(bytes, i, len, b'"', state),
            ScanState::LineComment => advance_line_comment(bytes, i),
            ScanState::BlockComment => advance_block_comment(bytes, i, len),
        };
        i = next_i;
        state = next_state;
    }

    scan.declared
}

/// Which parameter syntaxes beyond `:name` one [`find_param_occurrences`]
/// call additionally recognizes, resolved once per call from a driver id.
struct ScanConfig<'a> {
    allow_at: bool,
    allow_positional: bool,
    declared_at_names: &'a HashSet<String>,
}

/// Advance past one byte outside any quoted region or comment: recognizes
/// where a quoted region or comment starts, a Postgres `::` cast operator,
/// a `:name` parameter token, and (per `config`) an `@name` or positional
/// `?` token (returned as an occurrence, using `line` as its 1-based line).
fn advance_normal(
    sql: &str,
    bytes: &[u8],
    i: usize,
    len: usize,
    line: usize,
    config: &ScanConfig,
) -> (usize, ScanState, Option<ParamOccurrence>) {
    let b = bytes[i];
    if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
        return (i + 2, ScanState::LineComment, None);
    }
    if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
        return (i + 2, ScanState::BlockComment, None);
    }
    if b == b'\'' {
        return (i + 1, ScanState::SingleQuoted, None);
    }
    if b == b'"' {
        return (i + 1, ScanState::DoubleQuoted, None);
    }
    if b == b':' {
        if i + 1 < len && bytes[i + 1] == b':' {
            // Postgres cast operator (`total::numeric`), not a parameter.
            return (i + 2, ScanState::Normal, None);
        }
        if i + 1 < len && is_ident_start(bytes[i + 1]) {
            let mut j = i + 1;
            while j < len && is_ident_continue(bytes[j]) {
                j += 1;
            }
            let occurrence = ParamOccurrence {
                name: sql[i + 1..j].to_owned(),
                kind: ParamKind::Colon,
                offset: i,
                token_len: j - i,
                line,
            };
            return (j, ScanState::Normal, Some(occurrence));
        }
        return (i + 1, ScanState::Normal, None);
    }
    if config.allow_at && b == b'@' {
        if i + 1 < len && bytes[i + 1] == b'@' {
            // @@name (e.g. @@ROWCOUNT) is a system variable, never a
            // parameter.
            return (i + 2, ScanState::Normal, None);
        }
        if i + 1 < len && is_ident_start(bytes[i + 1]) {
            let mut j = i + 1;
            while j < len && is_ident_continue(bytes[j]) {
                j += 1;
            }
            let name = sql[i + 1..j].to_owned();
            if config
                .declared_at_names
                .contains(&name.to_ascii_lowercase())
            {
                return (j, ScanState::Normal, None);
            }
            let occurrence = ParamOccurrence {
                name,
                kind: ParamKind::At,
                offset: i,
                token_len: j - i,
                line,
            };
            return (j, ScanState::Normal, Some(occurrence));
        }
        return (i + 1, ScanState::Normal, None);
    }
    if config.allow_positional && b == b'?' {
        // A trailing digit run (e.g. sqlite's own numbered `?1` placeholder)
        // is consumed as part of this same token, so substitution never
        // leaves stray digits behind.
        let mut j = i + 1;
        while j < len && bytes[j].is_ascii_digit() {
            j += 1;
        }
        let occurrence = ParamOccurrence {
            name: String::new(),
            kind: ParamKind::Positional,
            offset: i,
            token_len: j - i,
            line,
        };
        return (j, ScanState::Normal, Some(occurrence));
    }
    (i + 1, ScanState::Normal, None)
}

/// Scan `sql` for every parameter token `driver_id` detects, in source
/// order: `:name` on every driver, `@name` only on `mssql` (excluding
/// `@@name` system variables and any name the script itself `DECLARE`s),
/// and a bare `?` only on `mysql` and `sqlite`. Skips a token inside a
/// single-quoted string literal, a double-quoted identifier, a `--` line
/// comment, a `/* */` block comment, or (for `:name`) a Postgres `::type`
/// cast (so `total::numeric` is never read as parameter `:numeric`).
#[must_use]
#[tracing::instrument(skip(sql), fields(driver_id = driver_id))]
pub fn find_param_occurrences(sql: &str, driver_id: &str) -> Vec<ParamOccurrence> {
    let allow_at = allows_at_params(driver_id);
    let declared_at_names = if allow_at {
        collect_declared_at_names(sql)
    } else {
        HashSet::new()
    };
    let config = ScanConfig {
        allow_at,
        allow_positional: allows_positional_params(driver_id),
        declared_at_names: &declared_at_names,
    };

    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut occurrences = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut state = ScanState::Normal;

    while i < len {
        if bytes[i] == b'\n' {
            line += 1;
        }
        let (next_i, next_state) = match state {
            ScanState::Normal => {
                let (next_i, next_state, occurrence) =
                    advance_normal(sql, bytes, i, len, line, &config);
                if let Some(occurrence) = occurrence {
                    occurrences.push(occurrence);
                }
                (next_i, next_state)
            }
            ScanState::SingleQuoted => advance_quoted(bytes, i, len, b'\'', state),
            ScanState::DoubleQuoted => advance_quoted(bytes, i, len, b'"', state),
            ScanState::LineComment => advance_line_comment(bytes, i),
            ScanState::BlockComment => advance_block_comment(bytes, i, len),
        };
        i = next_i;
        state = next_state;
    }

    occurrences
}

/// Words whose presence in a column name or parameter name suggests a
/// date/timestamp-shaped value.
const DATE_HINTS: [&str; 3] = ["date", "_at", "timestamp"];
/// Words suggesting a fractional numeric value.
const NUMERIC_HINTS: [&str; 5] = ["total", "amount", "price", "cost", "sum"];
/// Words suggesting a whole-number value.
const INTEGER_HINTS: [&str; 4] = ["limit", "count", "qty", "quantity"];

fn classify_hint(text: &str) -> Option<ParamType> {
    let lower = text.to_ascii_lowercase();
    if DATE_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Some(ParamType::Date);
    }
    if NUMERIC_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Some(ParamType::Numeric);
    }
    if INTEGER_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Some(ParamType::Integer);
    }
    None
}

/// A Postgres cast type name to the badge it implies, or `None` for a cast
/// this heuristic does not recognize.
fn classify_cast(cast: &str) -> Option<ParamType> {
    match cast.to_ascii_lowercase().as_str() {
        "numeric" | "decimal" | "real" | "float" | "float4" | "float8" => Some(ParamType::Numeric),
        "int" | "int2" | "int4" | "int8" | "integer" | "bigint" | "smallint" => {
            Some(ParamType::Integer)
        }
        "date" | "timestamp" | "timestamptz" | "time" | "timetz" => Some(ParamType::Date),
        "text" | "varchar" | "char" | "citext" => Some(ParamType::Text),
        _ => None,
    }
}

/// The identifier (possibly `alias.column`) immediately left of the last
/// comparison operator in `context`, if any: the rightmost-ending match
/// among `>=`, `<=`, `<>`, `!=`, `=`, `<`, `>`, so e.g. `>=` wins over the
/// `>` it contains.
fn column_before_operator(context: &str) -> Option<&str> {
    const OPERATORS: [&str; 7] = [">=", "<=", "<>", "!=", "=", "<", ">"];
    let trimmed = context.trim_end();
    let mut best: Option<(usize, usize)> = None;
    for op in OPERATORS {
        if let Some(start) = trimmed.rfind(op) {
            let end = start + op.len();
            if best.is_none_or(|(_, best_end)| end > best_end) {
                best = Some((start, end));
            }
        }
    }
    let (start, _) = best?;
    let before_op = trimmed[..start].trim_end();
    let word_start = before_op
        .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
        .map_or(0, |i| i + 1);
    let word = &before_op[word_start..];
    if word.is_empty() { None } else { Some(word) }
}

/// Heuristically infer `occurrence`'s type from its surrounding SQL text: a
/// `::type` cast immediately after it, a `LIMIT` keyword immediately
/// before it, the column compared against it on the same line, or (as a
/// last resort) hints in the parameter's own name, which never matches
/// for a positional `?`, since it has none. Falls back to
/// [`ParamType::Text`] when nothing more specific matches; no general SQL
/// type checker is implemented.
#[must_use]
fn infer_param_type(sql: &str, occurrence: &ParamOccurrence) -> ParamType {
    let token_end = occurrence.offset + occurrence.token_len;
    if let Some(after) = sql.get(token_end..)
        && let Some(rest) = after.strip_prefix("::")
    {
        let cast: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
        if let Some(inferred) = classify_cast(&cast) {
            return inferred;
        }
    }

    let before = &sql[..occurrence.offset];
    let trimmed_before = before.trim_end();
    let last_word = trimmed_before
        .rsplit(|c: char| !c.is_ascii_alphabetic())
        .next();
    if last_word.is_some_and(|word| word.eq_ignore_ascii_case("limit")) {
        return ParamType::Integer;
    }

    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let context = &sql[line_start..occurrence.offset];
    if let Some(column) = column_before_operator(context)
        && let Some(inferred) = classify_hint(column)
    {
        return inferred;
    }

    classify_hint(&occurrence.name).unwrap_or(ParamType::Text)
}

/// The key `detect_parameters` groups one occurrence's repeated uses under:
/// case-folded for [`ParamKind::At`], since T-SQL identifiers are
/// case-insensitive, and exact for every other kind.
fn group_key(occurrence: &ParamOccurrence, positional_label: &str) -> (ParamKind, String) {
    match occurrence.kind {
        ParamKind::Positional => (ParamKind::Positional, positional_label.to_owned()),
        ParamKind::At => (ParamKind::At, occurrence.name.to_ascii_lowercase()),
        ParamKind::Colon => (ParamKind::Colon, occurrence.name.clone()),
    }
}

/// Detect every parameter `driver_id` recognizes in `sql` (see
/// [`find_param_occurrences`]): a `:name` or `@name` occurrence collapses
/// with every repeated use of the same name into one logical [`Parameter`]
/// (matched case-insensitively for `@name`, displayed using its first
/// occurrence's own spelling), while a positional `?` occurrence is always
/// its own, labeled `?1`, `?2`, ... in occurrence order. The result is
/// ordered by each parameter's first-occurrence position, keeping every
/// occurrence's own line so the UI can show each use.
#[must_use]
#[tracing::instrument(skip(sql), fields(driver_id = driver_id))]
pub fn detect_parameters(sql: &str, driver_id: &str) -> Vec<Parameter> {
    let occurrences = find_param_occurrences(sql, driver_id);
    let mut order: Vec<(ParamKind, String)> = Vec::new();
    let mut grouped: HashMap<(ParamKind, String), (String, Vec<ParamOccurrence>)> = HashMap::new();
    let mut positional_count = 0usize;

    for occurrence in occurrences {
        let positional_label = if occurrence.kind == ParamKind::Positional {
            positional_count += 1;
            format!("?{positional_count}")
        } else {
            String::new()
        };
        let key = group_key(&occurrence, &positional_label);
        let display_name = if occurrence.kind == ParamKind::Positional {
            positional_label
        } else {
            occurrence.name.clone()
        };
        if !grouped.contains_key(&key) {
            order.push(key.clone());
        }
        let entry = grouped
            .entry(key)
            .or_insert_with(|| (display_name, Vec::new()));
        entry.1.push(occurrence);
    }

    order
        .into_iter()
        .filter_map(|key| {
            let (name, occurrences) = grouped.remove(&key)?;
            let inferred_type = occurrences
                .first()
                .map_or(ParamType::Text, |occ| infer_param_type(sql, occ));
            let (kind, _) = key;
            Some(Parameter {
                name,
                kind,
                occurrences,
                inferred_type,
            })
        })
        .collect()
}

/// Quote `value` as a SQL string literal for `driver_id`'s dialect: always
/// doubles an embedded single quote via [`quote_sql_string`], and for
/// [`MYSQL_DRIVER_ID`] also doubles an embedded backslash first, so a
/// value ending in (or containing) a backslash can never escape the
/// literal's closing quote.
fn quote_param_value(value: &str, driver_id: &str) -> String {
    if driver_id == MYSQL_DRIVER_ID {
        quote_sql_string(&value.replace('\\', "\\\\"))
    } else {
        quote_sql_string(value)
    }
}

/// Replace every occurrence of every parameter in `parameters` with its
/// value from `values` (keyed by each parameter's own
/// [`Parameter::storage_key`]), each rendered as an escaped SQL string
/// literal for `driver_id`'s dialect (see [`quote_param_value`]). A key
/// absent from `values` substitutes as an empty string literal.
///
/// This always renders a client-side string literal, never a real driver
/// bind parameter: `sql::Connection::stream_query` takes a plain `String`,
/// and no driver crate threads typed bind parameters through. The tradeoff
/// is that every value is escaped, not type-checked, by the database; the
/// inferred type badges shown alongside a parameter's input are advisory
/// display only and never change how its value is escaped here.
///
/// Escaping for every non-MySQL dialect assumes standard-conforming
/// string literals, where a backslash is a literal character; a server
/// configured otherwise (e.g. `standard_conforming_strings = off`) is out
/// of scope.
#[must_use]
#[tracing::instrument(skip(sql, parameters, values))]
// Every caller passes the standard hasher; generalizing over `BuildHasher`
// would add a type parameter with no real flexibility gained here.
#[allow(clippy::implicit_hasher)]
pub fn substitute_params(
    sql: &str,
    parameters: &[Parameter],
    values: &HashMap<String, String>,
    driver_id: &str,
) -> String {
    let mut occurrences: Vec<(String, &ParamOccurrence)> = parameters
        .iter()
        .flat_map(|parameter| {
            let key = parameter.storage_key();
            parameter
                .occurrences
                .iter()
                .map(move |occurrence| (key.clone(), occurrence))
        })
        .collect();
    occurrences.sort_by_key(|(_, occurrence)| std::cmp::Reverse(occurrence.offset));

    let mut result = sql.to_owned();
    for (key, occurrence) in occurrences {
        let end = occurrence.offset + occurrence.token_len;
        let value = values.get(&key).map_or("", String::as_str);
        let literal = quote_param_value(value, driver_id);
        result.replace_range(occurrence.offset..end, &literal);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        ParamKind, ParamType, Parameter, detect_parameters, find_param_occurrences,
        substitute_params,
    };

    const POSTGRES: &str = "postgres";
    const MYSQL: &str = "mysql";
    const SQLITE: &str = "sqlite";
    const MSSQL: &str = "mssql";

    #[test]
    fn a_query_with_no_parameters_finds_nothing() {
        let occurrences = find_param_occurrences("SELECT * FROM orders", POSTGRES);
        assert!(occurrences.is_empty());
    }

    #[test]
    fn finds_a_single_parameter_with_its_offset_and_line() {
        let sql = "SELECT * FROM orders WHERE status = :status";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].name, "status");
        assert_eq!(occurrences[0].line, 1);
        assert_eq!(
            &sql[occurrences[0].offset..occurrences[0].offset + 7],
            ":status"
        );
    }

    #[test]
    fn finds_multiple_distinct_parameters_across_lines_with_correct_line_numbers() {
        let sql =
            "SELECT *\nFROM orders\nWHERE created_at >= :start_date\n  AND created_at < :end_date";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["start_date", "end_date"]);
        assert_eq!(occurrences[0].line, 3);
        assert_eq!(occurrences[1].line, 4);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_single_quoted_string_literal() {
        let sql = "SELECT 'not :a_param here' AS label WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn a_string_literal_with_a_doubled_quote_still_closes_correctly() {
        let sql = "SELECT 'it''s :not_a_param' WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_line_comment() {
        let sql = "SELECT 1 -- :not_a_param\nWHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_block_comment() {
        let sql = "SELECT 1 /* :not_a_param\nspans lines */ WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_double_quoted_identifier() {
        let sql = "SELECT \"a:b\" FROM t WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn a_double_quoted_identifier_with_a_doubled_quote_still_closes_correctly() {
        let sql = "SELECT \"weird\"\"ident:x\" FROM t WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn a_postgres_cast_is_never_read_as_a_parameter() {
        let sql = "SELECT total::numeric FROM orders WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql, POSTGRES);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn colon_name_detection_fires_identically_regardless_of_driver_id() {
        let sql = "SELECT total::numeric FROM orders WHERE status = :status AND x = 'not :fake'";
        let baseline = find_param_occurrences(sql, POSTGRES);
        for driver_id in [MYSQL, SQLITE, MSSQL, "unknown"] {
            assert_eq!(
                find_param_occurrences(sql, driver_id),
                baseline,
                "colon-name detection must not vary for driver id {driver_id}"
            );
        }
    }

    #[test]
    fn detect_parameters_collapses_repeated_uses_of_the_same_name_into_one_parameter() {
        let sql = "SELECT * FROM orders WHERE status = :status OR prior_status = :status";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].name, "status");
        assert_eq!(parameters[0].occurrences.len(), 2);
    }

    #[test]
    fn detect_parameters_preserves_first_occurrence_order_for_distinct_names() {
        let sql = "WHERE created_at >= :start_date AND created_at < :end_date AND status = :status";
        let parameters = detect_parameters(sql, POSTGRES);
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["start_date", "end_date", "status"]);
    }

    #[test]
    fn infers_date_from_a_created_at_style_column_comparison() {
        let sql = "SELECT * FROM orders WHERE o.created_at >= :start_date";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn infers_numeric_from_a_total_style_column_comparison() {
        let sql = "SELECT * FROM orders WHERE o.total >= :min_total";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Numeric);
    }

    #[test]
    fn infers_integer_from_a_limit_clause() {
        let sql = "SELECT * FROM orders ORDER BY created_at DESC LIMIT :row_limit";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn infers_numeric_from_an_explicit_cast() {
        let sql = "SELECT * FROM orders WHERE o.total >= :amount::numeric";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Numeric);
    }

    #[test]
    fn infers_date_from_the_parameter_name_alone_when_no_context_matches() {
        let sql = "SELECT * FROM orders WHERE x = :start_date";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn infers_integer_from_the_parameter_name_alone_when_no_context_matches() {
        let sql = "SELECT * FROM orders WHERE x = :retry_count";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn infers_integer_from_an_explicit_int_cast() {
        let sql = "SELECT * FROM t WHERE x = :v::int";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn infers_date_from_an_explicit_date_cast() {
        let sql = "SELECT * FROM t WHERE x = :v::date";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn infers_text_from_an_explicit_text_cast() {
        let sql = "SELECT * FROM t WHERE x = :v::text";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    #[test]
    fn an_unrecognized_cast_falls_through_to_the_other_inference_heuristics() {
        let sql = "SELECT * FROM orders WHERE o.created_at >= :cutoff::uuid";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn an_unrecognized_cast_with_no_other_signal_falls_back_to_text() {
        let sql = "SELECT * FROM t WHERE x = :v::uuid";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    #[test]
    fn falls_back_to_text_when_nothing_more_specific_is_inferred() {
        let sql = "SELECT * FROM orders WHERE o.status = :status";
        let parameters = detect_parameters(sql, POSTGRES);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    #[test]
    fn substitute_params_replaces_every_occurrence_of_a_repeated_name() {
        let sql = "WHERE status = :status OR prior_status = :status";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        values.insert("status".to_owned(), "shipped".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(
            result,
            "WHERE status = 'shipped' OR prior_status = 'shipped'"
        );
    }

    #[test]
    fn substitute_params_preserves_surrounding_sql_across_multiple_distinct_parameters() {
        let sql = "SELECT * FROM orders WHERE created_at >= :start_date AND status = :status LIMIT :row_limit";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        values.insert("start_date".to_owned(), "2026-01-01".to_owned());
        values.insert("status".to_owned(), "shipped".to_owned());
        values.insert("row_limit".to_owned(), "50".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(
            result,
            "SELECT * FROM orders WHERE created_at >= '2026-01-01' AND status = 'shipped' LIMIT '50'"
        );
    }

    #[test]
    fn substitute_params_with_an_empty_values_map_substitutes_an_empty_string_literal() {
        let sql = "WHERE status = :status";
        let parameters = detect_parameters(sql, POSTGRES);
        let values = HashMap::new();
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, "WHERE status = ''");
    }

    #[test]
    fn substitute_params_escapes_an_embedded_single_quote_so_it_cannot_break_out_of_the_literal() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), "O'Brien".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, "WHERE name = 'O''Brien'");
    }

    #[test]
    fn substitute_params_is_safe_against_a_single_quote_semicolon_drop_table_shaped_payload() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), "x'; DROP TABLE users; --".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, "WHERE name = 'x''; DROP TABLE users; --'");
        assert_eq!(result.matches("DROP TABLE").count(), 1);
        // The payload lands entirely inside one quoted literal: no
        // unescaped quote reopens or closes the string early.
        assert_eq!(result.matches('\'').count(), 4);
    }

    #[test]
    fn substitute_params_on_mysql_doubles_a_trailing_backslash_so_it_cannot_escape_the_closing_quote()
     {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), r"x\".to_owned());
        let result = substitute_params(sql, &parameters, &values, MYSQL);
        assert_eq!(
            result, r"WHERE name = 'x\\'",
            "a trailing backslash must be doubled, not left free to escape the closing quote"
        );
    }

    #[test]
    fn substitute_params_on_mysql_is_safe_against_a_backslash_quote_injection_payload() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        // Under MySQL's default backslash-escaped string literals, an
        // undoubled backslash here would make the following quote an
        // escaped literal quote rather than the string's close, letting
        // ` OR 1=1 -- ` run as live SQL outside the literal.
        values.insert("name".to_owned(), r"x\' OR 1=1 -- ".to_owned());
        let result = substitute_params(sql, &parameters, &values, MYSQL);
        assert_eq!(result, r"WHERE name = 'x\\'' OR 1=1 -- '");
    }

    #[test]
    fn substitute_params_on_a_non_mysql_dialect_never_doubles_a_backslash() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql, POSTGRES);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), r"C:\data".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(
            result, r"WHERE name = 'C:\data'",
            "a standard-conforming backend must see the backslash unchanged"
        );
    }

    #[test]
    fn at_name_is_not_detected_on_postgres_mysql_or_sqlite_but_is_detected_on_mssql() {
        let sql = "SELECT * FROM orders WHERE start_date >= @start_date";
        for driver_id in [POSTGRES, MYSQL, SQLITE] {
            let occurrences = find_param_occurrences(sql, driver_id);
            assert!(
                occurrences.is_empty(),
                "@name must not be detected on {driver_id}"
            );
        }
        let occurrences = find_param_occurrences(sql, MSSQL);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].name, "start_date");
        assert_eq!(occurrences[0].kind, ParamKind::At);
    }

    #[test]
    fn at_rowcount_is_never_detected_on_mssql() {
        let sql = "SELECT @@ROWCOUNT AS affected";
        let occurrences = find_param_occurrences(sql, MSSQL);
        assert!(
            occurrences.is_empty(),
            "@@ROWCOUNT must never be read as a parameter"
        );
    }

    #[test]
    fn a_declared_at_variable_and_its_later_uses_are_excluded_on_mssql_while_an_undeclared_one_is_detected()
     {
        let sql = "DECLARE @total INT = 0;\n\
                    SELECT @total = SUM(amount) FROM orders WHERE created_at >= @start_date;\n\
                    SET @total = @total + 1;";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["start_date"],
            "every use of the declared @total must be excluded, leaving only @start_date"
        );
    }

    #[test]
    fn a_declared_at_variable_is_excluded_even_when_its_declare_statement_has_no_terminating_semicolon()
     {
        let sql = "DECLARE @total INT\nSET @total = 0\nSELECT amount, @start_date FROM orders";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["start_date"],
            "leaving the DECLARE statement without a ';' must not leak into treating the next \
             statement's comma as part of the DECLARE list"
        );
    }

    #[test]
    fn every_name_in_a_comma_separated_declare_list_is_excluded_on_mssql() {
        let sql =
            "DECLARE @a INT, @b INT;\nSELECT * FROM orders WHERE a = @a AND b = @b AND c = @c";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["c"],
            "every name in one DECLARE list must be excluded, leaving only the undeclared @c"
        );
    }

    #[test]
    fn a_declared_variables_parenthesized_type_list_does_not_end_the_declare_list_early() {
        let sql = "DECLARE @n DECIMAL(10,2), @b INT;\n\
                    SELECT * FROM orders WHERE n = @n AND b = @b AND c = @c";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["c"],
            "the comma inside DECIMAL(10,2) must not be read as the DECLARE list's own \
             separator, or @b would be wrongly treated as undeclared"
        );
    }

    #[test]
    fn a_bare_at_sign_not_followed_by_an_identifier_is_skipped_as_one_byte_on_mssql() {
        let sql = "SELECT 1 WHERE a @ b AND x = @flag";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["flag"],
            "a stray '@' with no identifier after it must not consume or hide the real \
             @flag token that follows"
        );
    }

    #[test]
    fn a_declared_variable_is_matched_case_insensitively_against_its_later_uses() {
        let sql = "DECLARE @Total INT = 0;\nSET @total = @total + 1;";
        let occurrences = find_param_occurrences(sql, MSSQL);
        assert!(
            occurrences.is_empty(),
            "a declared name must exclude a differently-cased later use, T-SQL identifiers \
             being case-insensitive by default"
        );
    }

    #[test]
    fn the_word_declare_inside_a_string_or_comment_does_not_suppress_detection() {
        let sql = "SELECT 'DECLARE' AS k, 1 -- DECLARE\nWHERE x = @flag";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["flag"],
            "DECLARE appearing only inside a string or comment must not exclude @flag"
        );
    }

    #[test]
    fn a_mixed_colon_and_at_name_script_on_mssql_orders_parameters_by_occurrence() {
        let sql = "WHERE a = @cutoff AND b = :status AND c = @cutoff";
        let parameters = detect_parameters(sql, MSSQL);
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        let kinds: Vec<ParamKind> = parameters.iter().map(|p| p.kind).collect();
        assert_eq!(names, vec!["cutoff", "status"]);
        assert_eq!(kinds, vec![ParamKind::At, ParamKind::Colon]);
    }

    #[test]
    fn a_colon_and_at_parameter_sharing_the_same_identifier_on_mssql_are_kept_distinct_by_storage_key()
     {
        let sql = "WHERE status = :status OR legacy_status = @status";
        let parameters = detect_parameters(sql, MSSQL);
        assert_eq!(parameters.len(), 2);
        let keys: Vec<String> = parameters.iter().map(Parameter::storage_key).collect();
        assert_eq!(
            keys,
            vec!["status".to_owned(), "@status".to_owned()],
            "an @name parameter's storage key must be @-prefixed so it cannot collide with a \
             :name of the same identifier"
        );

        let mut values = HashMap::new();
        values.insert("status".to_owned(), "shipped".to_owned());
        values.insert("@status".to_owned(), "legacy_shipped".to_owned());
        let result = substitute_params(sql, &parameters, &values, MSSQL);
        assert_eq!(
            result, "WHERE status = 'shipped' OR legacy_status = 'legacy_shipped'",
            "each of the two same-named parameters must fill from its own value"
        );
    }

    #[test]
    fn two_differently_cased_at_name_occurrences_collapse_into_one_parameter_on_mssql() {
        let sql = "WHERE a = @Cutoff AND b = @cutoff";
        let parameters = detect_parameters(sql, MSSQL);
        assert_eq!(
            parameters.len(),
            1,
            "T-SQL identifiers are case-insensitive, so @Cutoff and @cutoff name one variable"
        );
        assert_eq!(parameters[0].name, "Cutoff");
        assert_eq!(parameters[0].occurrences.len(), 2);
    }

    #[test]
    fn substitute_params_never_touches_an_at_name_on_a_driver_that_does_not_detect_it() {
        let sql = "SELECT * FROM orders WHERE start_date >= @start_date";
        let parameters = detect_parameters(sql, POSTGRES);
        assert!(parameters.is_empty());
        let values = HashMap::new();
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, sql);
    }

    #[test]
    fn bare_question_mark_is_not_detected_on_postgres_or_mssql_but_is_detected_on_mysql_and_sqlite()
    {
        let sql = "SELECT * FROM orders WHERE data ? 'key' AND data ?| ARRAY['a'] AND data ?& ARRAY['b'] AND status = ?";
        for driver_id in [POSTGRES, MSSQL] {
            let occurrences = find_param_occurrences(sql, driver_id);
            assert!(
                occurrences.is_empty(),
                "a bare ? must not be detected on {driver_id}, including jsonb operator forms"
            );
        }
        for driver_id in [MYSQL, SQLITE] {
            let occurrences = find_param_occurrences(sql, driver_id);
            let count = occurrences
                .iter()
                .filter(|o| o.kind == ParamKind::Positional)
                .count();
            assert_eq!(
                count, 4,
                "every ? (including inside ?| and ?&) is detected on {driver_id}"
            );
        }
    }

    #[test]
    fn a_question_mark_inside_a_string_or_comment_is_excluded_on_a_gated_driver() {
        let sql = "SELECT 'not ? a param', 1 -- ? not here\n/* ? not here either */ WHERE x = ?";
        let occurrences = find_param_occurrences(sql, MYSQL);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].kind, ParamKind::Positional);
    }

    #[test]
    fn repeated_question_marks_are_distinct_parameters_in_occurrence_order() {
        let sql = "WHERE status = ? OR prior_status = ?";
        let parameters = detect_parameters(sql, MYSQL);
        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].name, "?1");
        assert_eq!(parameters[1].name, "?2");
        assert_eq!(parameters[0].occurrences.len(), 1);
        assert_eq!(parameters[1].occurrences.len(), 1);
    }

    #[test]
    fn repeated_question_marks_with_identical_surrounding_sql_are_still_distinct_parameters() {
        let sql = "WHERE x = ? OR x = ?";
        let parameters = detect_parameters(sql, MYSQL);
        assert_eq!(
            parameters.len(),
            2,
            "two ? occurrences must never merge just because their surrounding SQL matches"
        );
        assert_eq!(parameters[0].name, "?1");
        assert_eq!(parameters[1].name, "?2");
    }

    #[test]
    fn a_bare_question_mark_immediately_followed_by_digits_consumes_them_as_one_token() {
        let sql = "WHERE id = ?1";
        let occurrences = find_param_occurrences(sql, SQLITE);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(
            occurrences[0].token_len, 2,
            "the ?1 token must cover both bytes"
        );

        let parameters = detect_parameters(sql, SQLITE);
        let mut values = HashMap::new();
        values.insert("?1".to_owned(), "5".to_owned());
        let result = substitute_params(sql, &parameters, &values, SQLITE);
        assert_eq!(
            result, "WHERE id = '5'",
            "substitution must not leave the trailing digit behind as stray text"
        );
    }

    #[test]
    fn substitution_fills_each_repeated_question_mark_independently() {
        let sql = "WHERE status = ? OR prior_status = ?";
        let parameters = detect_parameters(sql, MYSQL);
        let mut values = HashMap::new();
        values.insert("?1".to_owned(), "shipped".to_owned());
        values.insert("?2".to_owned(), "pending".to_owned());
        let result = substitute_params(sql, &parameters, &values, MYSQL);
        assert_eq!(
            result,
            "WHERE status = 'shipped' OR prior_status = 'pending'"
        );
    }

    #[test]
    fn a_mixed_colon_and_question_mark_script_on_mysql_orders_parameters_by_occurrence() {
        let sql = "WHERE status = ? AND created_at >= :start_date AND prior_status = ?";
        let parameters = detect_parameters(sql, MYSQL);
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["?1", "start_date", "?2"]);
    }

    #[test]
    fn a_positional_parameter_falls_back_to_text_when_no_heuristic_matches() {
        let sql = "WHERE status = ?";
        let parameters = detect_parameters(sql, MYSQL);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    #[test]
    fn a_positional_parameter_still_infers_integer_from_a_limit_clause() {
        let sql = "SELECT * FROM orders ORDER BY created_at DESC LIMIT ?";
        let parameters = detect_parameters(sql, MYSQL);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn a_positional_parameter_still_infers_date_from_a_created_at_style_column_comparison() {
        let sql = "SELECT * FROM orders WHERE o.created_at >= ?";
        let parameters = detect_parameters(sql, MYSQL);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn an_unresolved_driver_id_falls_back_to_colon_name_only_detection() {
        let sql =
            "SELECT * FROM orders WHERE start_date >= @start_date AND status = ? AND id = :id";
        let parameters = detect_parameters(sql, "unknown");
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["id"]);
    }

    /// Documents a known limitation (see this module's own doc comment):
    /// detection has no notion of a MySQL backtick-quoted identifier, so a
    /// `?`-shaped byte inside one is still read as a parameter.
    #[test]
    fn a_question_mark_inside_a_mysql_backtick_quoted_identifier_is_still_detected() {
        let sql = "SELECT `col?` FROM t WHERE x = ?";
        let occurrences = find_param_occurrences(sql, MYSQL);
        let count = occurrences
            .iter()
            .filter(|o| o.kind == ParamKind::Positional)
            .count();
        assert_eq!(count, 2, "the backtick-quoted ? is not currently exempted");
    }

    /// Documents a known limitation (see this module's own doc comment):
    /// detection has no notion of a T-SQL bracket-quoted identifier, so an
    /// `@name`-shaped run inside one is still read as a parameter.
    #[test]
    fn an_at_name_inside_a_mssql_bracket_quoted_identifier_is_still_detected() {
        let sql = "SELECT [col@name] FROM t WHERE x = @flag";
        let occurrences = find_param_occurrences(sql, MSSQL);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["name", "flag"],
            "the bracket-quoted @name is not currently exempted"
        );
    }
}
