//! App-specific spacing/size tokens for the workspace's panes, matching the
//! locked visual spec. Centralized here so no view hardcodes a raw pixel
//! literal inline. The base color palette and small reusable grid/tree/
//! scrollbar builders live in the `zsql-ui` crate instead -- see
//! `zsql_ui::colors`, `zsql_ui::grid`, `zsql_ui::tree`, and
//! `zsql_ui::scrollbar`.

use gpui::{Pixels, px};
use zsql_ui::colors;

/// Status-bar "connecting" indicator: a connection attempt is in flight.
pub const STATUS_CONNECTING: u32 = 0xd9_a2_5a;
/// Status-bar "error" indicator: connecting or the last query failed.
pub const STATUS_ERROR: u32 = 0xe2_6d_78;
/// Status-bar "disconnected" indicator: the liveliness probe reports the
/// connection unreachable. Deliberately its own constant (even though it
/// currently shares `STATUS_ERROR`'s hue) so the two can diverge without
/// hunting down every call site.
pub const STATUS_DISCONNECTED: u32 = 0xe2_6d_78;
/// Status-bar "limited" indicator: the last query was cancelled after its
/// result reached the configured row limit. Its own amber, distinct from
/// `STATUS_CONNECTING`'s, so a truncated result cannot be mistaken for a
/// connection attempt in flight.
pub const STATUS_LIMITED: u32 = 0xe8_a1_3a;

/// Height of the workspace header above the active tab's content, holding
/// the pane label and the Run button.
pub const WORKSPACE_HEADER_HEIGHT: Pixels = px(38.0);
/// Horizontal padding inside the workspace header.
pub const WORKSPACE_HEADER_PADDING_X: f32 = 10.0;
/// Text size of the workspace header's left-hand pane label.
pub const WORKSPACE_HEADER_LABEL_TEXT_SIZE: f32 = 10.5;
/// Height of the Run button.
pub const RUN_BUTTON_HEIGHT: Pixels = px(25.0);
/// Horizontal padding inside the Run button.
pub const RUN_BUTTON_PADDING_X: f32 = 11.0;
/// Corner radius of the Run button.
pub const RUN_BUTTON_RADIUS: f32 = 5.0;
/// Text size of the Run button's label.
pub const RUN_BUTTON_TEXT_SIZE: f32 = 11.5;
/// Size of the Run button's play icon.
pub const RUN_BUTTON_ICON_SIZE: Pixels = px(12.0);
/// Text size of the Run button's keyboard-shortcut hint.
pub const RUN_BUTTON_HINT_TEXT_SIZE: f32 = 10.0;
/// Run button background when hovered: a lighter teal than the resting accent.
pub const RUN_BUTTON_HOVER_BG: u32 = 0x46_cf_ba;
/// Run button shortcut-hint color: the page ink at reduced opacity, so it
/// reads as secondary against the teal fill.
pub const RUN_BUTTON_HINT: u32 = 0x10_12_17_b3;

/// Height of the results header bar (row count + source label).
pub const RESULTS_BAR_HEIGHT: Pixels = px(32.0);
/// Height of the bottom connection/status bar.
pub const STATUS_BAR_HEIGHT: Pixels = px(26.0);

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

/// Height of the sidebar's "SCHEMA" header bar.
pub const SIDEBAR_HEADER_HEIGHT: Pixels = px(34.0);
/// Left padding for a catalog row (tree depth 0).
pub const SIDEBAR_INDENT_L0: f32 = 10.0;
/// Left padding for a schema row (tree depth 1).
pub const SIDEBAR_INDENT_L1: f32 = 24.0;
/// Left padding for a relation row (tree depth 2).
pub const SIDEBAR_INDENT_L2: f32 = 42.0;
/// Vertical padding above/below the scrollable tree body.
pub const SIDEBAR_TREE_PADDING_Y: f32 = 6.0;
/// Size of a catalog/schema row's leading icon.
pub const SIDEBAR_ROW_ICON_SIZE: Pixels = px(13.0);
/// Size of a relation row's kind icon (table/view/matview/partitioned).
pub const SIDEBAR_RELATION_ICON_SIZE: Pixels = px(12.0);

/// Width of the sidebar tree's scrollbar track and thumb.
pub const SIDEBAR_SCROLLBAR_WIDTH: Pixels = px(8.0);
/// Corner radius of the sidebar scrollbar thumb.
pub const SIDEBAR_SCROLLBAR_RADIUS: f32 = 4.0;
/// Resting fill of the sidebar scrollbar thumb: faint text color at ~40%
/// alpha.
pub const SIDEBAR_SCROLLBAR_THUMB: u32 = 0x59_60_6f_66;
/// Hovered fill of the sidebar scrollbar thumb.
pub const SIDEBAR_SCROLLBAR_THUMB_HOVER: u32 = 0x59_60_6f_99;
/// Distance between the sidebar scrollbar track and the tree's right edge.
pub const SIDEBAR_SCROLLBAR_GAP: Pixels = px(4.0);

/// Text size of the "SCHEMA" header label.
pub const SIDEBAR_HEADER_TEXT_SIZE: f32 = 10.5;

/// Background tint for the selected relation row: teal at low opacity
/// (`0x33c2ac` at ~10% alpha), matching the results grid's teal accent.
pub const SIDEBAR_SELECTED_BG: u32 = 0x33_c2_ac_1a;

/// Text size of the "Results" label and row count in the results header bar.
pub const RESULTS_TAB_TEXT_SIZE: f32 = 11.5;
/// Text size of the source/relation label in the results header bar.
pub const RESULTS_META_TEXT_SIZE: f32 = 11.0;
/// Text size of the bottom connection/status bar.
pub const STATUS_BAR_TEXT_SIZE: f32 = 10.5;

/// Height of a `Generated` tab's compact SQL strip, tall enough for one line
/// of monospace text plus the editor's own vertical padding.
pub const GENERATED_STRIP_HEIGHT: Pixels = px(46.0);
/// Background tint of a `Generated` tab's compact strip: teal at very low
/// opacity (`0x33c2ac` at ~4.5% alpha).
pub const GENERATED_STRIP_BG: u32 = 0x33_c2_ac_0b;
/// Left accent border color of a `Generated` tab's compact strip: the
/// mockup's `genstrip` left accent, `zsql_ui::colors::TEAL_DIM` (the tab
/// bar's dashed active underline is the brighter `colors::TEAL` instead).
pub const GENERATED_STRIP_ACCENT: u32 = colors::TEAL_DIM;
/// Horizontal padding around the generated strip's trailing "generated" tag
/// and hint text.
pub const GENERATED_STRIP_TRAILING_PADDING_X: f32 = 14.0;
/// Horizontal gap between the generated strip's trailing tag and hint text.
pub const GENERATED_STRIP_TRAILING_GAP: f32 = 10.0;
/// Text size of the generated strip's trailing hint ("edit to convert to a
/// script").
pub const GENERATED_HINT_TEXT_SIZE: f32 = 11.0;
/// Text size of a tab's leading table-icon glyph.
pub const TAB_ICON_TEXT_SIZE: f32 = 11.0;

/// Width of the centered connection-manager modal panel.
pub const MODAL_WIDTH: Pixels = px(468.0);
/// Corner radius of the modal panel.
pub const MODAL_RADIUS: f32 = 10.0;
/// Height of the modal's title bar.
pub const MODAL_HEAD_HEIGHT: Pixels = px(44.0);
/// Dimmed backdrop behind the modal: near-black at ~62% alpha
/// (`rgba(8,9,12,.62)`).
pub const MODAL_BACKDROP: u32 = 0x08_09_0c_9e;
/// Tallest the modal's connection list is allowed to grow before it scrolls.
pub const MODAL_LIST_MAX_HEIGHT: Pixels = px(300.0);
/// Corner radius of a connection-list row.
pub const MODAL_ROW_RADIUS: f32 = 7.0;
/// Background tint marking the currently-connected row in the modal list:
/// teal at low opacity (`0x33c2ac` at ~9% alpha).
pub const MODAL_ROW_ACTIVE_BG: u32 = 0x33_c2_ac_17;
/// Text size of a connection-list row's name.
pub const MODAL_ROW_NAME_TEXT_SIZE: f32 = 12.5;
/// Text size of a connection-list row's url.
pub const MODAL_ROW_URL_TEXT_SIZE: f32 = 10.5;
/// Text size of the "connected" label shown next to the active row's name.
pub const MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE: f32 = 9.5;
/// Vertical gap between a connection-list row's name line and its url line.
pub const MODAL_ROW_INNER_GAP: Pixels = px(3.0);

/// Size of the modal head's close icon.
pub const MODAL_CLOSE_ICON_SIZE: Pixels = px(13.0);
/// Size of a connection-list row's delete icon.
pub const MODAL_DELETE_ICON_SIZE: Pixels = px(13.0);
/// Size of the "Add connection" affordance's plus icon.
pub const MODAL_ADD_ICON_SIZE: Pixels = px(12.0);

/// `group()` name tying the modal close row's hitbox to its icon's
/// `group_hover` tint, so hovering anywhere in the row -- not just the
/// icon's own small hitbox -- lightens the close glyph.
pub const MODAL_CLOSE_HOVER_GROUP: &str = "connection-modal-close-hover";

// ---- schema tab / view -----------------------------------------------

/// Width of a right-click context menu.
pub const CONTEXT_MENU_WIDTH: Pixels = px(210.0);
/// Padding around a context menu's items.
pub const CONTEXT_MENU_PADDING: Pixels = px(5.0);
/// Corner radius of a context menu.
pub const CONTEXT_MENU_RADIUS: f32 = 8.0;
/// Height of one context menu item.
pub const CONTEXT_MENU_ITEM_HEIGHT: Pixels = px(28.0);
/// Horizontal padding inside a context menu item.
pub const CONTEXT_MENU_ITEM_PADDING_X: Pixels = px(9.0);
/// Corner radius of a context menu item.
pub const CONTEXT_MENU_ITEM_RADIUS: f32 = 5.0;
/// Text size of a context menu item's label.
pub const CONTEXT_MENU_ITEM_TEXT_SIZE: f32 = 12.0;
/// Height of a context menu's separator line.
pub const CONTEXT_MENU_SEPARATOR_HEIGHT: Pixels = px(1.0);
/// Vertical margin around a context menu separator.
pub const CONTEXT_MENU_SEPARATOR_MARGIN_Y: Pixels = px(5.0);

/// Padding around the schema tab's header meta strip.
pub const SCHEMA_HEAD_PADDING_X: Pixels = px(16.0);
/// Top padding of the schema tab's header meta strip.
pub const SCHEMA_HEAD_PADDING_TOP: Pixels = px(13.0);
/// Bottom padding of the schema tab's header meta strip.
pub const SCHEMA_HEAD_PADDING_BOTTOM: Pixels = px(12.0);
/// Text size of the schema tab's structure icon and qualified-name title.
pub const SCHEMA_TITLE_TEXT_SIZE: f32 = 15.0;
/// Text size of the schema tab's kind pill (e.g. "TABLE").
pub const SCHEMA_KIND_PILL_TEXT_SIZE: f32 = 9.5;
/// Border color of the schema tab's kind pill: teal at ~30% alpha.
pub const SCHEMA_KIND_PILL_BORDER: u32 = 0x33_c2_ac_4d;
/// Corner radius of the schema tab's kind pill.
pub const SCHEMA_KIND_PILL_RADIUS: f32 = 4.0;
/// Horizontal padding inside the schema tab's kind pill.
pub const SCHEMA_KIND_PILL_PADDING_X: Pixels = px(6.0);
/// Text size of the schema tab's header stat counts.
pub const SCHEMA_STATS_TEXT_SIZE: f32 = 11.0;
/// Horizontal gap between the schema tab's header stat counts.
pub const SCHEMA_STATS_GAP: Pixels = px(18.0);

/// Padding around the schema tab's scrollable body.
pub const SCHEMA_SCROLL_PADDING: Pixels = px(16.0);
/// Vertical gap between the schema tab's Columns/Indexes/Constraints
/// sections.
pub const SCHEMA_SECTION_GAP: Pixels = px(20.0);
/// Text size of a section label (e.g. "Columns").
pub const SCHEMA_SECTION_LABEL_TEXT_SIZE: f32 = 10.5;
/// Bottom margin under a section label, above its table.
pub const SCHEMA_SECTION_LABEL_MARGIN_BOTTOM: Pixels = px(8.0);
/// Corner radius of a section label's trailing count pill.
pub const SCHEMA_SECTION_COUNT_PILL_RADIUS: f32 = 20.0;
/// Horizontal padding inside a section label's trailing count pill.
pub const SCHEMA_SECTION_COUNT_PILL_PADDING_X: Pixels = px(7.0);

/// Width of a column row's left key-rail tick.
pub const SCHEMA_RAIL_WIDTH: Pixels = px(3.0);
/// Text size of a Keys-cell badge or link chip.
pub const SCHEMA_BADGE_TEXT_SIZE: f32 = 9.5;
/// Horizontal padding inside a Keys-cell badge or link chip.
pub const SCHEMA_BADGE_PADDING_X: Pixels = px(6.0);
/// Corner radius of a Keys-cell badge or link chip.
pub const SCHEMA_BADGE_RADIUS: f32 = 4.0;
/// Border color of the primary-key badge: teal at ~32% alpha.
pub const SCHEMA_BADGE_PK_BORDER: u32 = 0x33_c2_ac_52;
/// Border color of the unique badge: amber at ~32% alpha.
pub const SCHEMA_BADGE_UNIQUE_BORDER: u32 = 0xd9_a2_5a_52;
/// Border/background color of the foreign-key link chip: link hue at low
/// alpha.
pub const SCHEMA_BADGE_LINK_BORDER: u32 = 0x7f_9c_ff_4d;
/// Background fill of the foreign-key link chip: link hue at very low alpha.
pub const SCHEMA_BADGE_LINK_BG: u32 = 0x7f_9c_ff_12;
/// Text size of the foreign-key link chip, slightly larger than a plain
/// [`SCHEMA_BADGE_TEXT_SIZE`] badge so its arrow and target read clearly.
pub const SCHEMA_FK_CHIP_TEXT_SIZE: f32 = 11.0;
/// Corner radius of the foreign-key link chip.
pub const SCHEMA_FK_CHIP_RADIUS: f32 = 5.0;
/// Label text of the primary-key badge.
pub const SCHEMA_BADGE_PK_LABEL: &str = "PK";
/// Label text of the unique badge.
pub const SCHEMA_BADGE_UNIQUE_LABEL: &str = "UNIQUE";
/// Label text of the check badge.
pub const SCHEMA_BADGE_CHECK_LABEL: &str = "CHECK";
/// Arrow glyph leading a foreign-key link chip.
pub const SCHEMA_FK_ARROW: &str = "->";

/// Label shown in a column's Null cell when it is declared `NOT NULL`.
pub const SCHEMA_NOT_NULL_LABEL: &str = "not null";
/// Label shown in a column's Null cell when it may be null.
pub const SCHEMA_NULLABLE_LABEL: &str = "nullable";
/// Placeholder shown in a column's Default cell when it has no default.
pub const SCHEMA_DEFAULT_NONE_PLACEHOLDER: &str = "-";
/// Label shown in an index row's Unique cell when the index enforces
/// uniqueness.
pub const SCHEMA_INDEX_UNIQUE_LABEL: &str = "unique";

/// Fixed pixel widths of the Columns table's cells, in display order
/// (Column, Type, Null, Default, Keys). The Type column is wide enough to
/// show a full Postgres type name (e.g. `character varying(255)` or
/// `timestamp with time zone`) without clipping its type tag.
pub const SCHEMA_COLUMNS_WIDTHS: [Pixels; 5] =
    [px(190.0), px(210.0), px(90.0), px(220.0), px(220.0)];
/// Fixed pixel widths of the Indexes table's cells, in display order (Name,
/// Method, Unique, Definition).
pub const SCHEMA_INDEXES_WIDTHS: [Pixels; 4] = [px(220.0), px(100.0), px(80.0), px(360.0)];
/// Fixed pixel widths of the Constraints table's cells, in display order
/// (Name, Type, Definition).
pub const SCHEMA_CONSTRAINTS_WIDTHS: [Pixels; 3] = [px(260.0), px(140.0), px(360.0)];
