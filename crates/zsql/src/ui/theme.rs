//! Dark color palette and spacing tokens for the results grid, matching the
//! locked visual spec. Centralized here so no view hardcodes a raw hex or
//! pixel literal inline. Not a theming system - just named constants.

use gpui::{Pixels, px};

/// Window/page background.
pub const INK: u32 = 0x10_12_17;
/// Bars (title bar, results header bar) background.
pub const PANEL: u32 = 0x16_19_22;
/// Raised surfaces: the column-header row background.
pub const RAISE: u32 = 0x1c_20_29;
/// Standard hairline border color.
pub const LINE: u32 = 0x2a_2f_3b;
/// A softer hairline, used between body cells and header columns.
pub const LINE_SOFT: u32 = 0x22_26_2f;
/// Primary text color.
pub const TEXT: u32 = 0xdb_e0_ea;
/// Secondary/muted text (labels, timestamps).
pub const MUTED: u32 = 0x87_8e_9f;
/// Faint text: NULLs, row numbers, disabled-ish labels.
pub const FAINT: u32 = 0x59_60_6f;
/// Accent color: row counts, active affordances.
pub const TEAL: u32 = 0x33_c2_ac;
/// Numeric cell text.
pub const NUMBER: u32 = 0xcf_9b_e8;
/// JSON/JSONB cell text.
pub const JSON: u32 = 0x9f_b4_d8;
/// Fallback/attention color for values the formatter could not classify
/// more specifically (arrays, unmapped backend types).
pub const UNKNOWN: u32 = 0xe2_6d_78;
/// Boolean cell text.
pub const BOOL: u32 = 0xd9_a2_5a;
/// Raw-bytes cell text.
pub const BYTES: u32 = 0x2b_85_79;

/// Type-tag badge border: teal at low opacity (`0x33c2ac` at ~28% alpha).
pub const TYPE_TAG_BORDER: u32 = 0x33_c2_ac_47;
/// Status-bar "connecting" indicator: a connection attempt is in flight.
pub const STATUS_CONNECTING: u32 = 0xd9_a2_5a;
/// Status-bar "error" indicator: connecting or the last query failed.
pub const STATUS_ERROR: u32 = 0xe2_6d_78;

/// Height of the results header bar (row count + source label).
pub const RESULTS_BAR_HEIGHT: Pixels = px(32.0);
/// Height of the sticky column-header row.
pub const HEADER_ROW_HEIGHT: Pixels = px(28.0);
/// Height of each body row in the virtualized grid.
pub const BODY_ROW_HEIGHT: Pixels = px(24.0);
/// Height of the bottom connection/status bar.
pub const STATUS_BAR_HEIGHT: Pixels = px(26.0);
/// Diameter of the status bar's connection-state dot.
pub const STATUS_DOT_SIZE: f32 = 6.0;

/// Horizontal padding inside every grid cell.
pub const CELL_PADDING_X: f32 = 11.0;
/// Approximate advance width (px) of one monospace glyph at the grid's text
/// size; used to estimate column widths from cell content length so columns
/// stay aligned between the header row and every virtualized body row.
pub const CELL_CHAR_WIDTH: f32 = 7.2;
/// Extra width reserved in a header cell for the type-tag badge that sits
/// next to the column name.
pub const TYPE_TAG_EXTRA_WIDTH: f32 = 34.0;
/// Narrowest a data column is allowed to shrink to.
pub const MIN_COLUMN_WIDTH: f32 = 90.0;
/// Widest a data column is allowed to grow to before the grid relies on
/// horizontal scrolling instead of pushing columns further out.
pub const MAX_COLUMN_WIDTH: f32 = 320.0;
/// Narrowest the leading row-number column is allowed to shrink to.
pub const ROW_NUMBER_MIN_WIDTH: f32 = 80.0;

/// Fixed width of the schema sidebar.
pub const SIDEBAR_WIDTH: Pixels = px(300.0);
/// Height of the sidebar's "SCHEMA" header bar.
pub const SIDEBAR_HEADER_HEIGHT: Pixels = px(34.0);
/// Height of each row in the schema tree.
pub const SIDEBAR_ROW_HEIGHT: Pixels = px(26.0);
/// Left padding for a catalog row (tree depth 0).
pub const SIDEBAR_INDENT_L0: f32 = 10.0;
/// Left padding for a schema row (tree depth 1).
pub const SIDEBAR_INDENT_L1: f32 = 24.0;
/// Left padding for a relation row (tree depth 2).
pub const SIDEBAR_INDENT_L2: f32 = 42.0;
/// Width reserved for a row's disclosure glyph (`v`/`>`) or, for a
/// non-disclosable relation row, the equivalent blank space that keeps its
/// label aligned with its parent schema's children.
pub const SIDEBAR_DISCLOSURE_WIDTH: f32 = 10.0;
/// Horizontal gap between a tree row's disclosure glyph, label, and trailing
/// affordances.
pub const SIDEBAR_ROW_GAP: f32 = 7.0;
/// Vertical padding above/below the scrollable tree body.
pub const SIDEBAR_TREE_PADDING_Y: f32 = 6.0;

/// Text size of the "SCHEMA" header label.
pub const SIDEBAR_HEADER_TEXT_SIZE: f32 = 10.5;
/// Text size of a tree row's label and disclosure glyph.
pub const SIDEBAR_ROW_TEXT_SIZE: f32 = 12.5;
/// Text size of a relation row's kind label (table/view/matview/partitioned).
pub const SIDEBAR_KIND_TEXT_SIZE: f32 = 9.0;
/// Text size of a row's trailing column-count/relation-count affordance.
pub const SIDEBAR_META_TEXT_SIZE: f32 = 10.0;

/// Background tint for the selected relation row: teal at low opacity
/// (`0x33c2ac` at ~10% alpha), matching the results grid's teal accent.
pub const SIDEBAR_SELECTED_BG: u32 = 0x33_c2_ac_1a;

/// Text size of the "Results" label and row count in the results header bar.
pub const RESULTS_TAB_TEXT_SIZE: f32 = 11.5;
/// Text size of the source/relation label in the results header bar.
pub const RESULTS_META_TEXT_SIZE: f32 = 11.0;
/// Text size of the bottom connection/status bar.
pub const STATUS_BAR_TEXT_SIZE: f32 = 10.5;
/// Text size of a column header's type-name badge.
pub const TYPE_TAG_TEXT_SIZE: f32 = 9.5;
/// Horizontal padding inside a type-name badge.
pub const TYPE_TAG_PADDING_X: f32 = 4.0;
/// Corner radius of a type-name badge.
pub const TYPE_TAG_RADIUS: f32 = 4.0;

/// Height of the SQL editor pane above the results grid.
pub const EDITOR_HEIGHT: Pixels = px(500.0);
/// Height of the editor toolbar that holds the Run button.
pub const EDITOR_TOOLBAR_HEIGHT: Pixels = px(38.0);
/// Horizontal padding inside the editor toolbar.
pub const EDITOR_TOOLBAR_PADDING_X: f32 = 10.0;
/// Text size of the toolbar's left-hand pane label.
pub const EDITOR_TOOLBAR_LABEL_TEXT_SIZE: f32 = 10.5;
/// Height of the Run button.
pub const RUN_BUTTON_HEIGHT: Pixels = px(25.0);
/// Horizontal padding inside the Run button.
pub const RUN_BUTTON_PADDING_X: f32 = 11.0;
/// Corner radius of the Run button.
pub const RUN_BUTTON_RADIUS: f32 = 5.0;
/// Text size of the Run button's label.
pub const RUN_BUTTON_TEXT_SIZE: f32 = 11.5;
/// Text size of the Run button's keyboard-shortcut hint.
pub const RUN_BUTTON_HINT_TEXT_SIZE: f32 = 10.0;
/// Run button background when hovered: a lighter teal than the resting accent.
pub const RUN_BUTTON_HOVER_BG: u32 = 0x46_cf_ba;
/// Run button shortcut-hint color: the page ink at reduced opacity, so it
/// reads as secondary against the teal fill.
pub const RUN_BUTTON_HINT: u32 = 0x10_12_17_b3;
/// Font size of the editor's text and gutter line numbers.
pub const EDITOR_TEXT_SIZE: f32 = 13.0;
/// Height of each row of editor text, content plus line spacing.
pub const EDITOR_LINE_HEIGHT: f32 = 21.0;
/// Width of the line-number gutter.
pub const EDITOR_GUTTER_WIDTH: Pixels = px(44.0);
/// Horizontal padding inside a gutter line-number cell.
pub const EDITOR_GUTTER_PADDING_X: f32 = 12.0;
/// Horizontal padding inside the editor's text area.
pub const EDITOR_PADDING_X: f32 = 14.0;
/// Vertical padding above the first line and below the last line of the
/// editor's text area.
pub const EDITOR_PADDING_Y: f32 = 12.0;
/// Width of the text cursor.
pub const EDITOR_CURSOR_WIDTH: Pixels = px(2.0);
/// How far a selection highlight extends past the end of a line that isn't
/// the selection's last line, so a selected line break reads as selected
/// too instead of stopping abruptly at the last character.
pub const EDITOR_SELECTION_EOL_PAD: f32 = 8.0;
/// Background tint for the active selection highlight: teal at low opacity
/// (`0x33c2ac` at ~20% alpha).
pub const EDITOR_SELECTION_BG: u32 = 0x33_c2_ac_33;
