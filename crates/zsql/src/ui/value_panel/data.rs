//! Pure logic behind the results grid's value panel: which renderer a cell
//! gets (type-gated JSON, hex/base64 bytes, full-precision numbers, ...),
//! the JSON tree model (parse, path, child counts, color roles), and the
//! panel's own open/pin/mode/tree-navigation state

use std::collections::HashSet;
use std::fmt::Write as _;

use zsql_core::{ColumnMeta, Value};

use crate::config::ValuePanelConfig;
use crate::ui::format::ValueKind;

/// Which renderer a cell's value gets, and the modes that renderer offers.
/// A pure function of the `Value`'s own variant plus its column's
/// driver-reported type name -- never of the value's text content, so a
/// `Text` column holding JSON-looking text never renders as JSON (see
/// [`renderer_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererKind {
    /// `Value::Json`: the driver reported this column as json/jsonb.
    Json,
    /// `Value::Text` or `Value::Uuid`.
    Text,
    /// `Value::Bytes`.
    Bytes,
    /// `Value::Int`, `Value::Float`, or `Value::Numeric`.
    Number,
    /// `Value::Timestamp`.
    Timestamp,
    /// `Value::Bool`.
    Bool,
    /// `Value::Null`.
    Null,
    /// Anything else (`Value::Unknown`, `Value::Array`), carrying the
    /// driver's own type name for display.
    Unknown { type_name: String },
}

/// Select the renderer for `value`, given its column's `type_name`. Pure:
/// no `gpui` dependency, so this is unit-testable directly, matching the
/// `format_value`/`format.rs` precedent.
#[must_use]
pub fn renderer_for(value: &Value, type_name: &str) -> RendererKind {
    match value {
        Value::Null => RendererKind::Null,
        Value::Bool(_) => RendererKind::Bool,
        Value::Int(_) | Value::Float(_) | Value::Numeric(_) => RendererKind::Number,
        Value::Text(_) | Value::Uuid(_) => RendererKind::Text,
        Value::Bytes(_) => RendererKind::Bytes,
        Value::Timestamp(_) => RendererKind::Timestamp,
        Value::Json(_) => RendererKind::Json,
        Value::Array(_) | Value::Unknown(_) => RendererKind::Unknown {
            type_name: type_name.to_owned(),
        },
    }
}

/// The JSON renderer's view modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonMode {
    #[default]
    Tree,
    Pretty,
    Raw,
}

/// Every [`JsonMode`], in the order the sub-bar's mode switcher shows them.
pub const JSON_MODES: [JsonMode; 3] = [JsonMode::Tree, JsonMode::Pretty, JsonMode::Raw];

/// The bytes renderer's view modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BytesMode {
    #[default]
    Hex,
    Base64,
}

/// Every [`BytesMode`], in the order the sub-bar's mode switcher shows them.
pub const BYTES_MODES: [BytesMode; 2] = [BytesMode::Hex, BytesMode::Base64];

/// The timestamp renderer's view modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimestampMode {
    #[default]
    Raw,
    Utc,
}

/// Every [`TimestampMode`], in the order the sub-bar's mode switcher shows
/// them.
pub const TIMESTAMP_MODES: [TimestampMode; 2] = [TimestampMode::Raw, TimestampMode::Utc];

// ---------------------------------------------------------------------
// JSON tree model
// ---------------------------------------------------------------------

/// A parsed JSON document, structured for the tree/pretty views. Object
/// entries keep their source order (`Config`'s `serde_json` dependency
/// enables `preserve_order`), so the tree reads in the same order the
/// document was written in.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonNode {
    Object(Vec<(String, JsonNode)>),
    Array(Vec<JsonNode>),
    String(String),
    /// The number's own source formatting, kept as text to avoid
    /// re-rendering it through a lossy float round-trip.
    Number(String),
    Bool(bool),
    Null,
}

fn json_node_from_serde(value: serde_json::Value) -> JsonNode {
    match value {
        serde_json::Value::Null => JsonNode::Null,
        serde_json::Value::Bool(b) => JsonNode::Bool(b),
        serde_json::Value::Number(n) => JsonNode::Number(n.to_string()),
        serde_json::Value::String(s) => JsonNode::String(s),
        serde_json::Value::Array(items) => {
            JsonNode::Array(items.into_iter().map(json_node_from_serde).collect())
        }
        serde_json::Value::Object(map) => JsonNode::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_node_from_serde(v)))
                .collect(),
        ),
    }
}

/// Reconstruct a `serde_json::Value` from a [`JsonNode`], for Pretty mode's
/// `serde_json::to_string_pretty`
#[must_use]
pub fn json_node_to_serde(node: &JsonNode) -> serde_json::Value {
    match node {
        JsonNode::Object(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), json_node_to_serde(v)))
                .collect(),
        ),
        JsonNode::Array(items) => {
            serde_json::Value::Array(items.iter().map(json_node_to_serde).collect())
        }
        JsonNode::String(s) => serde_json::Value::String(s.clone()),
        JsonNode::Number(n) => serde_json::from_str(n).unwrap_or(serde_json::Value::Null),
        JsonNode::Bool(b) => serde_json::Value::Bool(*b),
        JsonNode::Null => serde_json::Value::Null,
    }
}

/// A JSON value that failed to parse: the message the panel's footer shows
/// (`"not valid JSON at byte {N}"`) and the byte offset it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonParseFailure {
    pub message: String,
    pub byte_offset: usize,
}

/// Convert `serde_json`'s 1-based `(line, column)` error position into a
/// byte offset into `text`. `serde_json` already reports `column` as a byte
/// offset within the line (not a `char` count), so this only adds the byte
/// offset of `line`'s own start; the `.min(this_line.len())` guards a
/// column past the line's end (e.g. an error at an unterminated line's
/// final byte) from landing outside the line.
fn byte_offset_for_line_col(text: &str, line: usize, column: usize) -> usize {
    let mut offset = 0usize;
    for (index, this_line) in text.split('\n').enumerate() {
        if index + 1 == line {
            return offset + column.saturating_sub(1).min(this_line.len());
        }
        offset += this_line.len() + 1;
    }
    offset
}

/// Parse `text` as JSON into a [`JsonNode`] tree, or a [`JsonParseFailure`]
/// naming the byte offset the parser gave up at. A json/jsonb cell that
/// fails this keeps Tree/Pretty visible in the mode switcher (disabled, not
/// hidden) and falls back to Raw
///
/// # Errors
/// Returns [`JsonParseFailure`] if `text` is not valid JSON.
pub fn parse_json(text: &str) -> Result<JsonNode, JsonParseFailure> {
    serde_json::from_str::<serde_json::Value>(text)
        .map(json_node_from_serde)
        .map_err(|err| {
            let byte_offset = byte_offset_for_line_col(text, err.line(), err.column());
            JsonParseFailure {
                message: format!("not valid JSON at byte {byte_offset}"),
                byte_offset,
            }
        })
}

/// The outcome of loading a JSON value's source text into the panel: parsed
/// into a tree, invalid, or too large to parse eagerly.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonLoad {
    Parsed(JsonNode),
    Invalid(JsonParseFailure),
    /// `text.len()` exceeded `ValuePanelConfig::json_eager_parse_threshold_bytes`:
    /// `preview` holds the first `json_oversized_preview_bytes` (at a valid
    /// UTF-8 boundary) for Raw display, and `total_bytes` the full length,
    /// so the panel can offer a "Load full value" action without ever
    /// blocking a render on a multi-megabyte parse.
    Oversized {
        preview: String,
        total_bytes: usize,
    },
}

/// Load `text` per `cfg`'s eager-parse threshold: parses fully at or under
/// the threshold, else returns a truncated [`JsonLoad::Oversized`] preview.
#[must_use]
pub fn load_json(text: &str, cfg: &ValuePanelConfig) -> JsonLoad {
    if text.len() > cfg.json_eager_parse_threshold_bytes {
        let mut end = cfg.json_oversized_preview_bytes.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return JsonLoad::Oversized {
            preview: text[..end].to_owned(),
            total_bytes: text.len(),
        };
    }
    load_json_full(text)
}

/// Parse `text` fully regardless of size, for the "Load full value" action.
/// The caller is responsible for running this off the render path (e.g. via
/// `gpui::Context::background_spawn`) so an oversized value cannot freeze
/// the UI thread.
#[must_use]
pub fn load_json_full(text: &str) -> JsonLoad {
    match parse_json(text) {
        Ok(node) => JsonLoad::Parsed(node),
        Err(failure) => JsonLoad::Invalid(failure),
    }
}

/// The tree's right-aligned child-count label for an object/array node
/// (`"6 keys"`, `"3 items"`), or `None` for a scalar leaf
#[must_use]
pub fn child_count_label(node: &JsonNode) -> Option<String> {
    match node {
        JsonNode::Object(entries) => Some(format!("{} keys", entries.len())),
        JsonNode::Array(items) => Some(format!("{} items", items.len())),
        JsonNode::String(_) | JsonNode::Number(_) | JsonNode::Bool(_) | JsonNode::Null => None,
    }
}

/// The [`ValueKind`] a scalar tree node colors as -- the same role
/// `results.rs::kind_color`/the grid uses, so a JSON tree's scalars never
/// drift from the grid's own value coloring. `None` for an object/array:
/// those color as structure (keys stay text-secondary), not data
#[must_use]
pub fn node_value_kind(node: &JsonNode) -> Option<ValueKind> {
    match node {
        JsonNode::String(_) => Some(ValueKind::Text),
        JsonNode::Number(_) => Some(ValueKind::Number),
        JsonNode::Bool(_) => Some(ValueKind::Bool),
        JsonNode::Null => Some(ValueKind::Null),
        JsonNode::Object(_) | JsonNode::Array(_) => None,
    }
}

/// One step of a JSONPath-style path into a parsed document: an object key
/// or an array index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// Render `segments` as the JSONPath-style string the panel footer shows
/// and the copy-path control writes to the clipboard, e.g.
/// `$.items[0].sku`
#[must_use]
pub fn json_path_string(segments: &[PathSegment]) -> String {
    let mut out = String::from("$");
    for segment in segments {
        match segment {
            PathSegment::Key(key) => {
                out.push('.');
                out.push_str(key);
            }
            PathSegment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
        }
    }
    out
}

/// Walk `path` from `root`, returning the node it addresses, or `None` if
/// any step does not exist (e.g. a stale selection after the underlying
/// document changed).
#[must_use]
pub fn node_at_path<'a>(root: &'a JsonNode, path: &[PathSegment]) -> Option<&'a JsonNode> {
    let mut current = root;
    for segment in path {
        current = match (current, segment) {
            (JsonNode::Object(entries), PathSegment::Key(key)) => {
                &entries.iter().find(|(k, _)| k == key)?.1
            }
            (JsonNode::Array(items), PathSegment::Index(index)) => items.get(*index)?,
            _ => return None,
        };
    }
    Some(current)
}

/// One row of the tree as flattened for rendering/keyboard navigation: its
/// path from the root and nesting depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: Vec<PathSegment>,
    pub depth: usize,
}

/// Flatten `root` into the rows the tree currently shows, honoring
/// `expanded` (a node's children are included only while its own path is a
/// member). The root itself is always the first row.
#[must_use]
pub fn visible_tree_rows(root: &JsonNode, expanded: &HashSet<Vec<PathSegment>>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut path = Vec::new();
    push_tree_rows(root, &mut path, 0, expanded, &mut rows);
    rows
}

fn push_tree_rows(
    node: &JsonNode,
    path: &mut Vec<PathSegment>,
    depth: usize,
    expanded: &HashSet<Vec<PathSegment>>,
    rows: &mut Vec<TreeRow>,
) {
    rows.push(TreeRow {
        path: path.clone(),
        depth,
    });
    if !expanded.contains(path.as_slice()) {
        return;
    }
    match node {
        JsonNode::Object(entries) => {
            for (key, child) in entries {
                path.push(PathSegment::Key(key.clone()));
                push_tree_rows(child, path, depth + 1, expanded, rows);
                path.pop();
            }
        }
        JsonNode::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(PathSegment::Index(index));
                push_tree_rows(child, path, depth + 1, expanded, rows);
                path.pop();
            }
        }
        JsonNode::String(_) | JsonNode::Number(_) | JsonNode::Bool(_) | JsonNode::Null => {}
    }
}

/// The path `rows` (in [`visible_tree_rows`] order) selects after moving
/// `delta` rows from `current` -- clamped to the first/last row, with no
/// wraparound. `rows` empty is a no-op (`current` unchanged, cloned).
#[must_use]
pub fn move_tree_selection(
    rows: &[TreeRow],
    current: &[PathSegment],
    delta: isize,
) -> Vec<PathSegment> {
    if rows.is_empty() {
        return current.to_vec();
    }
    let current_index = rows.iter().position(|row| row.path == current).unwrap_or(0);
    let next_index = current_index
        .saturating_add_signed(delta)
        .min(rows.len() - 1);
    rows[next_index].path.clone()
}

/// Render `bytes` as a hex dump: one row of `bytes_per_row` bytes per line,
/// an 8-digit hex offset, two-digit hex byte columns, and an ASCII gutter
/// (`.` for anything outside printable ASCII)
#[must_use]
pub fn format_hex_dump(bytes: &[u8], bytes_per_row: usize) -> String {
    let bytes_per_row = bytes_per_row.max(1);
    let mut out = String::new();
    for (row_index, chunk) in bytes.chunks(bytes_per_row).enumerate() {
        let offset = row_index * bytes_per_row;
        let _ = write!(out, "{offset:08x}  ");
        for slot in 0..bytes_per_row {
            match chunk.get(slot) {
                Some(byte) => {
                    let _ = write!(out, "{byte:02x} ");
                }
                None => out.push_str("   "),
            }
        }
        out.push_str(" |");
        for &byte in chunk {
            let ch = if byte.is_ascii_graphic() || byte == b' ' {
                byte as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push_str("|\n");
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// `value`'s exact numeric text, read directly from the `Value` rather than
/// through `format_value`
#[must_use]
pub fn number_raw_text(value: &Value) -> Option<String> {
    match value {
        Value::Int(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Numeric(text) => Some(text.clone()),
        _ => None,
    }
}

/// `value`'s timestamp text exactly as the driver returned it. `None` for a
/// non-timestamp `Value`.
#[must_use]
pub fn timestamp_raw_text(value: &Value) -> Option<&str> {
    match value {
        Value::Timestamp(text) => Some(text.as_str()),
        _ => None,
    }
}

/// `raw` (an ISO-8601 timestamp) re-rendered in UTC, or `None` if it does
/// not parse as either an RFC 3339 timestamp or a bare `YYYY-MM-DD HH:MM:SS`
/// form (the latter treated as already UTC, since it carries no offset of
/// its own to convert from).
#[must_use]
pub fn timestamp_utc_text(raw: &str) -> Option<String> {
    use chrono::{DateTime, NaiveDateTime, Utc};

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc).to_rfc3339());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            let dt = DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc);
            return Some(dt.to_rfc3339());
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValuePanelContent {
    pub id: usize,
    pub value: Value,
    pub column: ColumnMeta,
}

impl ValuePanelContent {
    pub fn new(id: usize, value: Value, column: ColumnMeta) -> Self {
        Self { id, value, column }
    }
}

// ---------------------------------------------------------------------
// Panel state: open/pin/expand, per-kind mode choices, tree navigation
// ---------------------------------------------------------------------

/// The value panel's own state: whether it is open, pinned to a cell, each
/// renderer's currently chosen mode, and the JSON tree's selection/expansion.
/// Holds no `gpui` types, so the panel's interaction logic is unit-testable
/// without a window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValuePanelState {
    content: Option<ValuePanelContent>,
    open: bool,
    pinned: bool,
    json_mode: JsonMode,
    bytes_mode: BytesMode,
    timestamp_mode: TimestampMode,
    tree_selected: Vec<PathSegment>,
    tree_expanded: HashSet<Vec<PathSegment>>,
}

impl ValuePanelState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    /// Close the panel and drop any pin: reopening later always starts
    /// following the grid's live selection again.
    pub fn close(&mut self) {
        self.open = false;
        self.pinned = false;
    }

    #[must_use]
    pub fn content(&self) -> Option<&ValuePanelContent> {
        self.content.as_ref()
    }

    pub fn set_content(&mut self, content: Option<ValuePanelContent>) {
        if self.is_pinned() {
            return;
        }
        self.content = content;
    }

    pub fn pin(&mut self) {
        self.pinned = true;
    }

    pub fn unpin(&mut self) {
        self.pinned = false;
    }

    pub fn toggle_pinned(&mut self) {
        if self.pinned {
            self.unpin();
        } else {
            self.pin();
        }
    }

    #[must_use]
    pub fn json_mode(&self) -> JsonMode {
        self.json_mode
    }

    pub fn set_json_mode(&mut self, mode: JsonMode) {
        self.json_mode = mode;
    }

    #[must_use]
    pub fn bytes_mode(&self) -> BytesMode {
        self.bytes_mode
    }

    pub fn set_bytes_mode(&mut self, mode: BytesMode) {
        self.bytes_mode = mode;
    }

    #[must_use]
    pub fn timestamp_mode(&self) -> TimestampMode {
        self.timestamp_mode
    }

    pub fn set_timestamp_mode(&mut self, mode: TimestampMode) {
        self.timestamp_mode = mode;
    }

    #[must_use]
    pub fn selected_tree_path(&self) -> &[PathSegment] {
        &self.tree_selected
    }

    pub fn select_tree_path(&mut self, path: Vec<PathSegment>) {
        self.tree_selected = path;
    }

    /// Reset the tree's selection/expansion, e.g. once the panel starts
    /// showing a different value: a stale selection or expand-set from a
    /// previous document would otherwise point at nodes the new one may not
    /// have.
    pub fn reset_tree(&mut self) {
        self.tree_selected.clear();
        self.tree_expanded.clear();
    }

    #[must_use]
    pub fn is_tree_node_expanded(&self, path: &[PathSegment]) -> bool {
        self.tree_expanded.contains(path)
    }

    /// Every currently-expanded node's path, for [`visible_tree_rows`].
    #[must_use]
    pub fn tree_expanded(&self) -> &HashSet<Vec<PathSegment>> {
        &self.tree_expanded
    }

    pub fn set_tree_node_expanded(&mut self, path: Vec<PathSegment>, expanded: bool) {
        if expanded {
            self.tree_expanded.insert(path);
        } else {
            self.tree_expanded.remove(&path);
        }
    }

    /// Expand the node at `path` if collapsed, else collapse it (the tree's
    /// left/right keyboard contract collapses/expands rather than toggling
    /// blindly, but a single shared toggle is exposed here for callers that
    /// already know which direction they want).
    pub fn toggle_tree_node(&mut self, path: &[PathSegment]) {
        let expanded = self.is_tree_node_expanded(path);
        self.set_tree_node_expanded(path.to_vec(), !expanded);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use zsql_core::value::UnknownValue;
    use zsql_core::{ColumnMeta, Value};

    use super::{
        BytesMode, JsonLoad, JsonMode, JsonNode, PathSegment, RendererKind, TimestampMode,
        ValuePanelContent, ValuePanelState, child_count_label, format_hex_dump, json_node_to_serde,
        json_path_string, load_json, move_tree_selection, node_at_path, node_value_kind,
        number_raw_text, parse_json, renderer_for, timestamp_raw_text, timestamp_utc_text,
        visible_tree_rows,
    };
    use crate::config::ValuePanelConfig;
    use crate::ui::format::ValueKind;

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: true,
        }
    }

    // ---- renderer_for (AC8, AC9) ----------------------------------

    #[test]
    fn renderer_for_maps_every_value_variant_to_its_documented_kind() {
        assert_eq!(renderer_for(&Value::Null, "text"), RendererKind::Null);
        assert_eq!(renderer_for(&Value::Bool(true), "bool"), RendererKind::Bool);
        assert_eq!(renderer_for(&Value::Int(1), "int8"), RendererKind::Number);
        assert_eq!(
            renderer_for(&Value::Float(1.5), "float8"),
            RendererKind::Number
        );
        assert_eq!(
            renderer_for(&Value::Numeric("1.5".to_owned()), "numeric"),
            RendererKind::Number
        );
        assert_eq!(
            renderer_for(&Value::Text("hi".to_owned()), "text"),
            RendererKind::Text
        );
        assert_eq!(
            renderer_for(&Value::Uuid("u".to_owned()), "uuid"),
            RendererKind::Text
        );
        assert_eq!(
            renderer_for(&Value::Bytes(vec![1]), "bytea"),
            RendererKind::Bytes
        );
        assert_eq!(
            renderer_for(
                &Value::Timestamp("2026-01-01T00:00:00Z".to_owned()),
                "timestamptz"
            ),
            RendererKind::Timestamp
        );
        assert_eq!(
            renderer_for(&Value::Json("{}".to_owned()), "jsonb"),
            RendererKind::Json
        );
        assert_eq!(
            renderer_for(
                &Value::Unknown(UnknownValue::Text("(1,2)".to_owned())),
                "point"
            ),
            RendererKind::Unknown {
                type_name: "point".to_owned()
            }
        );
        assert_eq!(
            renderer_for(&Value::Unknown(UnknownValue::None), "point"),
            RendererKind::Unknown {
                type_name: "point".to_owned()
            }
        );
        assert_eq!(
            renderer_for(&Value::Array(vec![Value::Int(1)]), "int4[]"),
            RendererKind::Unknown {
                type_name: "int4[]".to_owned()
            }
        );
    }

    #[test]
    fn renderer_for_distinguishes_null_from_empty_text() {
        assert_eq!(
            renderer_for(&Value::Null, "text"),
            RendererKind::Null,
            "a Null cell must render the explicit NULL state, not the Text renderer"
        );
        assert_eq!(
            renderer_for(&Value::Text(String::new()), "text"),
            RendererKind::Text,
            "an empty-string cell must still render as Text, not Null"
        );
    }

    #[test]
    fn renderer_for_never_offers_json_for_text_holding_json_looking_content() {
        // Type-gated, not content-sniffed: a Text value must render as Text
        // even when its content parses as JSON.
        let looks_like_json = Value::Text(r#"{"a":1}"#.to_owned());
        assert_eq!(
            renderer_for(&looks_like_json, "text"),
            RendererKind::Text,
            "a Text value must never be classified as Json regardless of its content"
        );
    }

    // ---- JSON parsing / failure byte offset (AC10) -----------------

    #[test]
    fn parse_json_reads_a_well_formed_document() {
        let node = parse_json(r#"{"a":1,"b":[true,null]}"#).expect("well-formed JSON must parse");
        match node {
            JsonNode::Object(entries) => assert_eq!(entries.len(), 2),
            other => panic!("expected an object, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_reports_the_byte_offset_of_a_malformed_document() {
        // serde_json reports the position it gave up at: having read "tru"
        // and expecting the rest of "true", the unexpected `}` is that
        // position -- the last byte of this single-line, all-ASCII input.
        let text = r#"{"valid": true, "bad": tru}"#;
        let failure = parse_json(text).expect_err("truncated `tru` is not valid JSON");
        let expected_offset = text.len() - 1;
        assert_eq!(failure.byte_offset, expected_offset);
        assert_eq!(
            failure.message,
            format!("not valid JSON at byte {expected_offset}")
        );
    }

    #[test]
    fn parse_json_reports_a_byte_offset_correctly_when_the_error_line_holds_multi_byte_utf8() {
        // serde_json's column is a byte offset within the line, not a char
        // count: a multi-byte character earlier on the same line must not
        // throw off the byte offset this reports. `text.len()` is itself a
        // byte length, so comparing against it (rather than counting chars)
        // gives an expected value that is correct independent of encoding.
        let text = "{\"k\u{e9}y\": tru}"; // key holds U+00E9, 2 UTF-8 bytes but 1 char
        let failure = parse_json(text).expect_err("truncated `tru` is not valid JSON");
        assert_eq!(failure.byte_offset, text.len() - 1);
    }

    // ---- eager parse threshold / oversized preview (AC11) ----------

    #[test]
    fn load_json_parses_fully_at_or_under_the_threshold() {
        let cfg = ValuePanelConfig {
            json_eager_parse_threshold_bytes: 1_000,
            ..ValuePanelConfig::default()
        };
        let text = r#"{"ok":true}"#;
        assert!(matches!(load_json(text, &cfg), JsonLoad::Parsed(_)));
    }

    #[test]
    fn load_json_returns_a_truncated_preview_past_the_threshold() {
        let cfg = ValuePanelConfig {
            json_eager_parse_threshold_bytes: 10,
            json_oversized_preview_bytes: 4,
            ..ValuePanelConfig::default()
        };
        let text = "0123456789ABCDEF"; // 16 bytes, over the 10-byte threshold
        match load_json(text, &cfg) {
            JsonLoad::Oversized {
                preview,
                total_bytes,
            } => {
                assert_eq!(preview, "0123");
                assert_eq!(total_bytes, 16);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    #[test]
    fn load_json_never_splits_the_preview_inside_a_utf8_character() {
        let cfg = ValuePanelConfig {
            json_eager_parse_threshold_bytes: 1,
            json_oversized_preview_bytes: 2,
            ..ValuePanelConfig::default()
        };
        // Each euro sign is 3 bytes; a naive byte-index slice at 2 would
        // split the first character and panic.
        let text = "\u{20AC}\u{20AC}\u{20AC}";
        match load_json(text, &cfg) {
            JsonLoad::Oversized { preview, .. } => assert_eq!(preview, ""),
            other => panic!("expected Oversized, got {other:?}"),
        }
    }

    // ---- child counts (AC13) ---------------------------------------

    #[test]
    fn child_count_label_reports_object_keys_and_array_items() {
        let doc = parse_json(r#"{"a":1,"b":2,"items":[1,2,3]}"#).unwrap();
        assert_eq!(child_count_label(&doc), Some("3 keys".to_owned()));
        let JsonNode::Object(entries) = &doc else {
            panic!("expected an object");
        };
        let items = &entries.iter().find(|(k, _)| k == "items").unwrap().1;
        assert_eq!(child_count_label(items), Some("3 items".to_owned()));
    }

    #[test]
    fn child_count_label_is_none_for_scalars() {
        assert_eq!(child_count_label(&JsonNode::Null), None);
        assert_eq!(child_count_label(&JsonNode::Bool(true)), None);
        assert_eq!(child_count_label(&JsonNode::Number("1".to_owned())), None);
        assert_eq!(child_count_label(&JsonNode::String("s".to_owned())), None);
    }

    // ---- scalar color roles reuse kind_color's mapping (AC12) ------

    #[test]
    fn node_value_kind_matches_the_grids_own_value_kind_per_scalar() {
        assert_eq!(
            node_value_kind(&JsonNode::String("s".to_owned())),
            Some(ValueKind::Text)
        );
        assert_eq!(
            node_value_kind(&JsonNode::Number("1".to_owned())),
            Some(ValueKind::Number)
        );
        assert_eq!(
            node_value_kind(&JsonNode::Bool(true)),
            Some(ValueKind::Bool)
        );
        assert_eq!(node_value_kind(&JsonNode::Null), Some(ValueKind::Null));
    }

    #[test]
    fn node_value_kind_is_none_for_objects_and_arrays_so_keys_stay_structural() {
        assert_eq!(node_value_kind(&JsonNode::Object(vec![])), None);
        assert_eq!(node_value_kind(&JsonNode::Array(vec![])), None);
    }

    // ---- path computation (AC14) ------------------------------------

    #[test]
    fn json_path_string_renders_nested_array_and_object_access() {
        let path = vec![
            PathSegment::Key("items".to_owned()),
            PathSegment::Index(0),
            PathSegment::Key("sku".to_owned()),
        ];
        assert_eq!(json_path_string(&path), "$.items[0].sku");
    }

    #[test]
    fn json_path_string_of_the_root_is_the_bare_dollar_sign() {
        assert_eq!(json_path_string(&[]), "$");
    }

    #[test]
    fn node_at_path_resolves_a_nested_array_and_object_case() {
        let doc = parse_json(r#"{"items":[{"sku":"A1"},{"sku":"B2"}]}"#).unwrap();
        let path = vec![
            PathSegment::Key("items".to_owned()),
            PathSegment::Index(1),
            PathSegment::Key("sku".to_owned()),
        ];
        assert_eq!(
            node_at_path(&doc, &path),
            Some(&JsonNode::String("B2".to_owned()))
        );
    }

    #[test]
    fn node_at_path_returns_none_for_a_path_that_does_not_exist() {
        let doc = parse_json(r#"{"a":1}"#).unwrap();
        let path = vec![PathSegment::Key("missing".to_owned())];
        assert_eq!(node_at_path(&doc, &path), None);
    }

    // ---- tree flattening / keyboard navigation (AC21) ---------------

    #[test]
    fn visible_tree_rows_includes_only_the_root_when_nothing_is_expanded() {
        let doc = parse_json(r#"{"a":1,"b":2}"#).unwrap();
        let rows = visible_tree_rows(&doc, &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, Vec::new());
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn visible_tree_rows_includes_children_once_their_parent_is_expanded() {
        let doc = parse_json(r#"{"a":1,"b":[true,false]}"#).unwrap();
        let mut expanded = HashSet::new();
        expanded.insert(Vec::new());
        expanded.insert(vec![PathSegment::Key("b".to_owned())]);
        let rows = visible_tree_rows(&doc, &expanded);
        // root, "a", "b", "b"[0], "b"[1]
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[1].path, vec![PathSegment::Key("a".to_owned())]);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(
            rows[4].path,
            vec![PathSegment::Key("b".to_owned()), PathSegment::Index(1)]
        );
        assert_eq!(rows[4].depth, 2);
    }

    #[test]
    fn move_tree_selection_steps_forward_and_backward_and_clamps() {
        let doc = parse_json(r#"{"a":1,"b":2,"c":3}"#).unwrap();
        let mut expanded = HashSet::new();
        expanded.insert(Vec::new());
        let rows = visible_tree_rows(&doc, &expanded);
        assert_eq!(rows.len(), 4);

        let first = move_tree_selection(&rows, &[], 1);
        assert_eq!(first, vec![PathSegment::Key("a".to_owned())]);

        let clamped_top = move_tree_selection(&rows, &[], -5);
        assert_eq!(clamped_top, Vec::<PathSegment>::new());

        let clamped_bottom = move_tree_selection(&rows, &[], 100);
        assert_eq!(clamped_bottom, vec![PathSegment::Key("c".to_owned())]);

        // With no visible rows the current path is returned untouched.
        let some_path = vec![PathSegment::Key("a".to_owned())];
        assert_eq!(move_tree_selection(&[], &some_path, 1), some_path);
    }

    // ---- hex dump / base64 (AC16) ------------------------------------

    #[test]
    fn format_hex_dump_lays_out_offset_hex_and_ascii_gutter() {
        let bytes = b"Hello, world!";
        let dump = format_hex_dump(bytes, 8);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("00000000  "));
        assert!(lines[0].contains("48 65 6c 6c 6f 2c 20 77"));
        assert!(lines[0].ends_with("|Hello, w|"));
        assert!(lines[1].starts_with("00000008  "));
        assert!(lines[1].ends_with("|orld!|"));
    }

    #[test]
    fn format_hex_dump_replaces_non_printable_bytes_with_a_dot_in_the_gutter() {
        let dump = format_hex_dump(&[0x00, 0x41, 0xff], 16);
        assert!(dump.ends_with("|.A.|"));
    }

    // ---- number / timestamp full precision (AC7, AC17, AC18) --------

    #[test]
    fn number_raw_text_preserves_a_numeric_string_longer_than_float_precision() {
        let exact = "123456789012345678901234567890.123456789012345";
        let text = number_raw_text(&Value::Numeric(exact.to_owned())).unwrap();
        assert_eq!(
            text, exact,
            "the exact source digits must survive unchanged"
        );
    }

    #[test]
    fn number_raw_text_renders_a_float_with_all_its_significant_digits() {
        let value = Value::Float(0.1 + 0.2); // 0.30000000000000004
        let text = number_raw_text(&value).unwrap();
        assert_eq!(text, "0.30000000000000004");
    }

    #[test]
    fn number_raw_text_is_none_for_non_numeric_values() {
        assert_eq!(number_raw_text(&Value::Text("1".to_owned())), None);
    }

    // ---- Null vs empty-string Text distinguishability (AC15) --------

    #[test]
    fn null_and_empty_string_text_are_distinct_renderer_states() {
        // A Null cell must never be confused with an empty-string Text cell:
        // they select different renderers.
        assert_eq!(renderer_for(&Value::Null, "text"), RendererKind::Null);
        assert_eq!(
            renderer_for(&Value::Text(String::new()), "text"),
            RendererKind::Text
        );
    }

    #[test]
    fn timestamp_raw_text_reproduces_the_drivers_exact_string() {
        let raw = "2026-07-14T09:12:31.123456+02:00";
        assert_eq!(
            timestamp_raw_text(&Value::Timestamp(raw.to_owned())),
            Some(raw)
        );
    }

    #[test]
    fn timestamp_utc_text_converts_a_fixed_offset_to_utc() {
        let utc = timestamp_utc_text("2026-07-14T09:12:31+02:00").unwrap();
        assert_eq!(utc, "2026-07-14T07:12:31+00:00");
    }

    #[test]
    fn timestamp_utc_text_treats_a_bare_naive_timestamp_as_already_utc() {
        let utc = timestamp_utc_text("2026-07-14T09:12:31").unwrap();
        assert_eq!(utc, "2026-07-14T09:12:31+00:00");
    }

    #[test]
    fn timestamp_utc_text_treats_a_space_separated_naive_timestamp_as_already_utc() {
        let utc = timestamp_utc_text("2026-07-14 09:12:31").unwrap();
        assert_eq!(utc, "2026-07-14T09:12:31+00:00");
    }

    #[test]
    fn timestamp_utc_text_is_none_for_text_that_is_not_a_timestamp() {
        assert_eq!(timestamp_utc_text("not a timestamp"), None);
    }

    #[test]
    fn json_node_to_serde_round_trips_through_a_pretty_print() {
        let doc = parse_json(r#"{"a":1,"b":[true,null,"s"]}"#).unwrap();
        let pretty = serde_json::to_string_pretty(&json_node_to_serde(&doc)).unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(
            reparsed,
            serde_json::json!({"a": 1, "b": [true, null, "s"]})
        );
    }

    // ---- panel state: open/close, pin freezes content (AC2, AC20) ---

    fn content(id: usize) -> ValuePanelContent {
        ValuePanelContent::new(id, Value::Int(1), column("c", "int8"))
    }

    #[test]
    fn a_fresh_panel_is_closed_and_unpinned() {
        let panel = ValuePanelState::new();
        assert!(!panel.is_open());
        assert!(!panel.is_pinned());
    }

    #[test]
    fn open_and_close_flip_whether_the_panel_is_visible() {
        let mut panel = ValuePanelState::new();
        panel.open();
        assert!(panel.is_open());
        panel.close();
        assert!(!panel.is_open());
    }

    #[test]
    fn an_unpinned_panel_updates_its_content_to_follow_the_grid() {
        let mut panel = ValuePanelState::new();
        panel.open();
        panel.set_content(Some(content(0)));
        assert_eq!(panel.content().map(|c| c.id), Some(0));
        panel.set_content(Some(content(1)));
        assert_eq!(
            panel.content().map(|c| c.id),
            Some(1),
            "an unpinned panel must retarget as the grid's selection moves"
        );
    }

    #[test]
    fn a_pinned_panel_ignores_content_updates_from_grid_focus_changes() {
        let mut panel = ValuePanelState::new();
        panel.open();
        panel.set_content(Some(content(0)));
        panel.pin();

        // A later selection change tries to retarget the panel; while pinned
        // it must keep showing its pinned content rather than silently
        // following the grid.
        panel.set_content(Some(content(1)));
        assert!(panel.is_pinned());
        assert_eq!(panel.content().map(|c| c.id), Some(0));
    }

    #[test]
    fn close_drops_the_pin_so_reopening_follows_the_grid_again() {
        let mut panel = ValuePanelState::new();
        panel.open();
        panel.set_content(Some(content(2)));
        panel.pin();
        panel.close();
        assert!(!panel.is_pinned());
        // With the pin dropped, content updates take effect once more.
        panel.set_content(Some(content(5)));
        assert_eq!(panel.content().map(|c| c.id), Some(5));
    }

    #[test]
    fn toggle_pinned_pins_the_current_content_then_unpins() {
        let mut panel = ValuePanelState::new();
        panel.open();
        panel.set_content(Some(content(4)));
        panel.toggle_pinned();
        assert!(panel.is_pinned());
        panel.set_content(Some(content(0)));
        assert_eq!(panel.content().map(|c| c.id), Some(4));

        panel.toggle_pinned();
        assert!(!panel.is_pinned());
        panel.set_content(Some(content(0)));
        assert_eq!(panel.content().map(|c| c.id), Some(0));
    }

    // ---- panel mode + tree state accessors ---------------------------

    #[test]
    fn mode_setters_are_read_back_exactly() {
        let mut panel = ValuePanelState::new();
        assert_eq!(panel.json_mode(), JsonMode::Tree);
        panel.set_json_mode(JsonMode::Pretty);
        assert_eq!(panel.json_mode(), JsonMode::Pretty);

        assert_eq!(panel.bytes_mode(), BytesMode::Hex);
        panel.set_bytes_mode(BytesMode::Base64);
        assert_eq!(panel.bytes_mode(), BytesMode::Base64);

        assert_eq!(panel.timestamp_mode(), TimestampMode::Raw);
        panel.set_timestamp_mode(TimestampMode::Utc);
        assert_eq!(panel.timestamp_mode(), TimestampMode::Utc);
    }

    #[test]
    fn tree_node_expansion_toggles_and_reset_clears_selection_and_expansion() {
        let mut panel = ValuePanelState::new();
        let path = vec![PathSegment::Key("a".to_owned())];
        assert!(!panel.is_tree_node_expanded(&path));
        panel.toggle_tree_node(&path);
        assert!(panel.is_tree_node_expanded(&path));
        panel.toggle_tree_node(&path);
        assert!(!panel.is_tree_node_expanded(&path));

        panel.set_tree_node_expanded(path.clone(), true);
        panel.select_tree_path(path.clone());
        panel.reset_tree();
        assert!(!panel.is_tree_node_expanded(&path));
        assert_eq!(panel.selected_tree_path(), &[] as &[PathSegment]);
    }
}
