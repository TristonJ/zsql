//! App-specific spacing/size tokens for the workspace's panes, matching the
//! locked visual spec, plus this app's own derivations from the shared
//! [`zsql_ui::theme::Theme`] for washes/borders that used to be baked ARGB
//! constants. Centralized here so no view hardcodes a raw pixel or color
//! literal inline. The base color palette and small reusable grid/tree/
//! scrollbar builders live in the `zsql-ui` crate instead -- see
//! `zsql_ui::theme`, `zsql_ui::grid`, `zsql_ui::tree`, and
//! `zsql_ui::scrollbar`.

use gpui::{Pixels, px};
use zsql_ui::theme::{Colors, Theme};

/// Status-bar "disconnected" indicator: the liveliness probe reports the
/// connection unreachable. Deliberately its own function (even though it
/// currently shares [`Colors::status_error`]'s hue) so the two can diverge
/// without hunting down every call site.
#[must_use]
pub fn status_disconnected(theme: &Theme) -> u32 {
    theme.colors.status_error
}

/// Run button background when hovered: a lighter teal than the resting
/// accent, additively lightened rather than mixed toward another role so it
/// stays recognizably the accent's own hue.
#[must_use]
pub fn run_button_hover_bg(theme: &Theme) -> u32 {
    Colors::lighten(theme.colors.accent, 19, 13, 14)
}

/// Run button shortcut-hint color: the page background at reduced opacity,
/// so it reads as secondary against the accent fill.
#[must_use]
pub fn run_button_hint(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.bg_app, 0xb3)
}

/// Run button background while no live connection is held: a dimmer accent
/// than the resting fill, so the disabled state still reads as the same
/// control rather than a different color role entirely.
#[must_use]
pub fn run_button_disabled_bg(theme: &Theme) -> u32 {
    theme.colors.accent_dim()
}

/// Height of the workspace header above the active tab's content, holding
/// the pane label and the Run button.
pub const WORKSPACE_HEADER_HEIGHT: Pixels = px(38.0);
/// Horizontal padding inside the workspace header.
pub const WORKSPACE_HEADER_PADDING_X: f32 = 10.0;
/// Text size of the workspace header's left-hand pane label.
pub const WORKSPACE_HEADER_LABEL_TEXT_SIZE: f32 = 10.5;
pub const TAB_SCROLLBAR_TRACK_WIDTH: f32 = 4.0;

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

/// Height of the results header bar (row count + source label).
pub const RESULTS_BAR_HEIGHT: Pixels = px(32.0);
/// Height of the bottom connection/status bar.
pub const STATUS_BAR_HEIGHT: Pixels = px(26.0);
/// Maximum width of the connection panel in the status bar
pub const STATUS_BAR_CONNECTION_MAX_WIDTH: Pixels = px(300.0);

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
/// Extra padding chars reserved in the header cells for the sort affordance
/// & fn indicator
pub const HEADER_EXTRA_PADDING_CHARS: usize = 4;

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

/// Left padding for a script/library row: the tree's top-level (catalog-row)
/// indent, since a script row has no nesting hierarchy of its own.
pub const SIDEBAR_SCRIPT_ROW_INDENT: f32 = SIDEBAR_INDENT_L0;

/// Width of the sidebar tree's scrollbar track and thumb.
pub const SIDEBAR_SCROLLBAR_WIDTH: Pixels = px(8.0);
/// Corner radius of the sidebar scrollbar thumb.
pub const SIDEBAR_SCROLLBAR_RADIUS: f32 = 4.0;
/// Distance between the sidebar scrollbar track and the tree's right edge.
pub const SIDEBAR_SCROLLBAR_GAP: Pixels = px(4.0);

/// Text size of the "SCHEMA"/"SCRIPTS" pane tab labels and their trailing
/// mono count.
pub const SIDEBAR_HEADER_TEXT_SIZE: f32 = 10.5;

// ---- sidebar pane switcher (the "SCHEMA"/"SCRIPTS" tabs in the header) --

/// Horizontal padding inside a pane tab, and the pane switcher's trailing
/// tail's own right padding (matching the tabs' own edge inset).
pub const SIDEBAR_PANE_TAB_PADDING_X: Pixels = px(14.0);
/// Horizontal gap between a pane tab's label and its trailing count.
pub const SIDEBAR_PANE_TAB_GAP: Pixels = px(7.0);

// ---- sidebar database row (its own full-width row under the pane tabs) -

/// Height of the sidebar's database row.
pub const SIDEBAR_DB_ROW_HEIGHT: Pixels = px(30.0);
/// Horizontal padding inside the database row.
pub const SIDEBAR_DB_ROW_PADDING_X: Pixels = px(14.0);
/// Horizontal gap between the database row's eyebrow label, current
/// database name, and trailing chevron.
pub const SIDEBAR_DB_ROW_GAP: Pixels = px(8.0);
/// Text size of the database row's "DB" eyebrow label.
pub const SIDEBAR_DB_ROW_EYEBROW_TEXT_SIZE: f32 = 9.0;
/// Text size of the database row's current-database name.
pub const SIDEBAR_DB_ROW_NAME_TEXT_SIZE: f32 = 11.5;
/// Size of the database row's trailing chevron glyph.
pub const SIDEBAR_DB_ROW_CHEVRON_ICON_SIZE: Pixels = px(9.0);

// ---- scripts pane: group labels and the library open-dot ----------------

/// Height of a scripts-pane group label ("This connection - name" /
/// "Library").
pub const SIDEBAR_SCRIPT_GROUP_HEIGHT: Pixels = px(24.0);
/// Top margin above a scripts-pane group label, separating it from
/// whatever renders above it.
pub const SIDEBAR_SCRIPT_GROUP_MARGIN_TOP: Pixels = px(8.0);
/// Horizontal padding inside a scripts-pane group label.
pub const SIDEBAR_SCRIPT_GROUP_PADDING_X: Pixels = px(14.0);
/// Text size of a scripts-pane group label.
pub const SIDEBAR_SCRIPT_GROUP_TEXT_SIZE: f32 = 9.0;
/// Horizontal gap between a group label and its trailing connection-name
/// suffix.
pub const SIDEBAR_SCRIPT_GROUP_SUFFIX_GAP: Pixels = px(5.0);
/// Diameter of the accent dot marking a library script currently open as a
/// tab on this connection.
pub const SIDEBAR_LIBRARY_OPEN_DOT_SIZE: Pixels = px(5.0);
/// Horizontal gap before a library row's open-dot.
pub const SIDEBAR_LIBRARY_OPEN_DOT_GAP: Pixels = px(6.0);

// ---- scripts pane: pinned "open external file" footer -------------------

/// Fully transparent. For borders that reserve their space until a hover
/// state paints them, so hovering never shifts layout.
pub const COLOR_TRANSPARENT: u32 = 0x0000_0000;
/// Height of the scripts pane's pinned open-external-file footer.
pub const SIDEBAR_SCRIPTS_FOOTER_HEIGHT: Pixels = px(40.0);
/// Horizontal padding inside the footer, around its button row.
pub const SIDEBAR_SCRIPTS_FOOTER_PADDING_X: Pixels = px(7.0);
/// Height of the footer's button row.
pub const SIDEBAR_SCRIPTS_FOOTER_ROW_HEIGHT: Pixels = px(28.0);
/// Corner radius of the footer's button row.
pub const SIDEBAR_SCRIPTS_FOOTER_ROW_RADIUS: f32 = 6.0;
/// Gap between the button row's icon, label, and shortcut chip.
pub const SIDEBAR_SCRIPTS_FOOTER_GAP: Pixels = px(7.0);
/// Size of the button row's leading icon.
pub const SIDEBAR_SCRIPTS_FOOTER_ICON_SIZE: Pixels = px(13.0);
/// Text size of the footer row's trailing shortcut chip.
pub const SIDEBAR_SCRIPTS_FOOTER_SHORTCUT_TEXT_SIZE: f32 = 10.0;
/// Horizontal padding inside the shortcut chip.
pub const SIDEBAR_SCRIPTS_FOOTER_SHORTCUT_PADDING_X: Pixels = px(3.0);
/// Corner radius of the shortcut chip.
pub const SIDEBAR_SCRIPTS_FOOTER_SHORTCUT_RADIUS: f32 = 4.0;

// ---- scripts pane: empty connection-group invitation ---------------------

/// Horizontal margin around the empty-state invitation block.
pub const SIDEBAR_SCRIPTS_EMPTY_MARGIN_X: Pixels = px(14.0);
/// Top margin above the empty-state invitation block.
pub const SIDEBAR_SCRIPTS_EMPTY_MARGIN_TOP: Pixels = px(10.0);
/// Bottom margin below the empty-state invitation block.
pub const SIDEBAR_SCRIPTS_EMPTY_MARGIN_BOTTOM: Pixels = px(4.0);
/// Horizontal padding inside the empty-state invitation block.
pub const SIDEBAR_SCRIPTS_EMPTY_PADDING_X: Pixels = px(12.0);
/// Vertical padding inside the empty-state invitation block.
pub const SIDEBAR_SCRIPTS_EMPTY_PADDING_Y: Pixels = px(16.0);
/// Corner radius of the empty-state invitation block.
pub const SIDEBAR_SCRIPTS_EMPTY_RADIUS: f32 = 8.0;
/// Vertical gap between the empty-state block's two lines.
pub const SIDEBAR_SCRIPTS_EMPTY_GAP: Pixels = px(6.0);
/// Text size of the empty-state block's first line.
pub const SIDEBAR_SCRIPTS_EMPTY_TITLE_TEXT_SIZE: f32 = 12.0;
/// Text size of the empty-state block's second line.
pub const SIDEBAR_SCRIPTS_EMPTY_DETAIL_TEXT_SIZE: f32 = 11.0;
/// Horizontal gap between the empty-state block's shortcut chip and the
/// rest of its second line.
pub const SIDEBAR_SCRIPTS_EMPTY_KBD_GAP: Pixels = px(4.0);
/// Text size of the empty-state block's shortcut chip.
pub const SIDEBAR_SCRIPTS_EMPTY_KBD_TEXT_SIZE: f32 = 10.0;
/// Horizontal padding inside the empty-state block's shortcut chip.
pub const SIDEBAR_SCRIPTS_EMPTY_KBD_PADDING_X: Pixels = px(4.0);
/// Corner radius of the empty-state block's shortcut chip.
pub const SIDEBAR_SCRIPTS_EMPTY_KBD_RADIUS: f32 = 4.0;

/// The platform's save shortcut label
#[must_use]
pub const fn save_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+S"
    } else {
        "Ctrl+S"
    }
}

/// Platform-specific save-as shortcut label
#[must_use]
pub const fn save_as_shortcut_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+Shift+S"
    } else {
        "Ctrl+Shift+S"
    }
}

/// Background tint for the selected relation row: the accent color at low
/// opacity.
#[must_use]
pub fn sidebar_selected_bg(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.accent, 0x1a)
}

/// Text size of the "Results" label and row count in the results header bar.
pub const RESULTS_TAB_TEXT_SIZE: f32 = 11.5;
/// Text size of the source/relation label in the results header bar.
pub const RESULTS_META_TEXT_SIZE: f32 = 11.0;
/// Text size of the bottom connection/status bar.
pub const STATUS_BAR_TEXT_SIZE: f32 = 10.5;
/// The width of the query messages in the status bar area
pub const STATUS_BAR_QUERY_MESSAGE_WIDTH: Pixels = px(150.0);

// ---- results pane: Text view -------------------------------------------

/// Height of one line row in the Text view's document body and gutter.
pub const TEXT_VIEW_LINE_HEIGHT: Pixels = px(21.0);
/// Font size of the Text view's document body and gutter line numbers.
pub const TEXT_VIEW_FONT_SIZE: f32 = 12.5;
/// Horizontal gap between the results bar's trailing copy/wrap buttons and
/// the Grid|Text view switch.
pub const RESULTS_BAR_RIGHT_GAP: Pixels = px(9.0);
/// Extra horizontal room reserved past the Text view's longest line when
/// wrap is off, so its virtualized body list's horizontal scroll extent
/// leaves the last character clear of the scrollable edge.
pub const TEXT_VIEW_CONTENT_EXTENT_SLACK: Pixels = px(24.0);

/// Horizontal padding inside the results bar's copy/wrap icon buttons.
pub const RESULTS_ICON_BUTTON_PADDING_X: Pixels = px(5.0);
/// Vertical padding inside the results bar's copy/wrap icon buttons.
pub const RESULTS_ICON_BUTTON_PADDING_Y: Pixels = px(3.0);
/// Corner radius of the results bar's copy/wrap icon buttons.
pub const RESULTS_ICON_BUTTON_RADIUS: f32 = 5.0;
/// Text size of the results bar's copy/wrap icon buttons.
pub const RESULTS_ICON_BUTTON_TEXT_SIZE: f32 = 11.0;

// ---- results pane: preview sort/pager controls -------------------------

/// Height of a pager control (first/prev/next/last, page-size cycle).
pub const PAGER_BUTTON_HEIGHT: Pixels = px(20.0);
/// Horizontal padding inside a pager control.
pub const PAGER_BUTTON_PADDING_X: Pixels = px(7.0);
/// Corner radius of a pager control.
pub const PAGER_BUTTON_RADIUS: f32 = 5.0;
/// Text size of every pager control and the "rows X-Y" / "page N / total"
/// readouts.
pub const PAGER_TEXT_SIZE: f32 = 11.0;
/// Horizontal gap between the pager's own controls.
pub const PAGER_GROUP_GAP: Pixels = px(6.0);
/// Text/border opacity applied to a pager control while it is disabled
/// (the boundary end it steps toward, or the active tab is not a live
/// generated preview).
pub const PAGER_DISABLED_OPACITY: f32 = 0.45;

/// Background of the Grid|Text view switch's active segment, and of the
/// wrap toggle when wrap is on: the accent wash used elsewhere for a
/// selected/active control.
#[must_use]
pub fn view_switch_active_bg(theme: &Theme) -> gpui::Rgba {
    theme.colors.accent_wash()
}

/// Text color of the Grid|Text view switch's active segment, and of the
/// wrap toggle when wrap is on.
#[must_use]
pub fn view_switch_active_text(theme: &Theme) -> u32 {
    theme.colors.accent_strong()
}

/// Background of the Text view's selected-line highlight: the same accent
/// wash the SQL editor pane uses for its own text selection.
#[must_use]
pub fn text_selection_bg(theme: &Theme) -> gpui::Rgba {
    theme.colors.accent_wash_hover()
}

/// Height of a `Generated` tab's compact SQL strip showing one line of
/// monospace text plus the editor's own vertical padding.
pub const GENERATED_STRIP_HEIGHT: Pixels = px(46.0);

/// The most lines a `Generated` strip grows to fit; taller queries scroll
/// inside the editor instead.
pub const GENERATED_STRIP_MAX_LINES: usize = 6;

/// Height of a `Generated` strip whose query spans `line_count` lines: the
/// one-line base plus one editor line per extra line, capped at
/// [`GENERATED_STRIP_MAX_LINES`].
#[must_use]
#[allow(clippy::cast_precision_loss)] // capped at GENERATED_STRIP_MAX_LINES
pub fn generated_strip_height(line_count: usize) -> Pixels {
    let lines = line_count.clamp(1, GENERATED_STRIP_MAX_LINES);
    GENERATED_STRIP_HEIGHT + px(zsql_editor::EDITOR_LINE_HEIGHT) * (lines - 1) as f32
}

/// Background tint of a `Generated` tab's compact strip: the accent color
/// at very low opacity.
#[must_use]
pub fn generated_strip_bg(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.accent, 0x0b)
}

/// Left accent border color of a `Generated` tab's compact strip: a dimmer
/// teal than the tab bar's own dashed active underline, which stays the
/// full accent. Reuses the raw-bytes value role rather than a fresh field,
/// since the two happen to share this dimmer-teal hue.
#[must_use]
pub fn generated_strip_accent(theme: &Theme) -> u32 {
    theme.colors.value_bytes
}

// ---- results pane: filter bar -------------------------------------------

/// Minimum height of the filter bar strip under the results bar. Grows past
/// this (via `flex_wrap`) once enough chips no longer fit one row.
pub const FILTER_BAR_MIN_HEIGHT: Pixels = px(34.0);
/// Horizontal gap between the filter bar's own chips/connectors/controls.
pub const FILTER_BAR_GAP: Pixels = px(6.0);
/// Text size of the filter bar's "FILTER" label, and every chip/connector.
pub const FILTER_BAR_TEXT_SIZE: f32 = 11.5;

/// Height of a committed filter chip.
pub const FILTER_CHIP_HEIGHT: Pixels = px(24.0);
/// Corner radius of a filter chip.
pub const FILTER_CHIP_RADIUS: f32 = 5.0;
/// Horizontal padding inside a filter chip.
pub const FILTER_CHIP_PADDING_X: Pixels = px(8.0);
/// Horizontal gap between a chip's own column/operator/value/remove parts.
pub const FILTER_CHIP_INNER_GAP: Pixels = px(6.0);
/// A committed chip's own right padding, past its trailing remove control
/// (which carries its own tighter clickable margin, unlike
/// [`FILTER_CHIP_PADDING_X`] on the left).
pub const FILTER_CHIP_PADDING_RIGHT: Pixels = px(4.0);
/// Side length of a chip's remove control.
pub const FILTER_CHIP_REMOVE_SIZE: Pixels = px(16.0);
/// Corner radius of a chip's remove control.
pub const FILTER_CHIP_REMOVE_RADIUS: f32 = 3.0;
/// Size of the chip's remove control's "X" glyph.
pub const FILTER_CHIP_REMOVE_ICON_SIZE: Pixels = px(10.0);
/// Horizontal gap between an expression-classified value's text and its
/// trailing `fx` tag.
pub const FILTER_VALUE_EXPRESSION_GAP: Pixels = px(4.0);

/// Height of the AND/OR connector pill between two chips.
pub const FILTER_CONNECTOR_HEIGHT: Pixels = px(20.0);
/// Horizontal padding inside the AND/OR connector pill.
pub const FILTER_CONNECTOR_PADDING_X: Pixels = px(7.0);
/// Corner radius of the AND/OR connector pill.
pub const FILTER_CONNECTOR_RADIUS: f32 = 4.0;

/// Height of the "+ filter" and "clear all" controls.
pub const FILTER_CONTROL_HEIGHT: Pixels = px(24.0);
/// Horizontal padding inside the "+ filter" control.
pub const FILTER_ADD_PADDING_X: Pixels = px(9.0);
/// Corner radius of the "+ filter" control.
pub const FILTER_ADD_RADIUS: f32 = 5.0;
/// Horizontal gap between the "+ filter" control's own "+" and "filter"
/// parts.
pub const FILTER_ADD_CONTROL_GAP: Pixels = px(5.0);
/// Corner radius of the "clear all" control.
pub const FILTER_CLEAR_ALL_RADIUS: f32 = 4.0;

/// Width of the value text field shown while a chip is being edited.
pub const FILTER_VALUE_FIELD_WIDTH: Pixels = px(120.0);

/// Text size of the `fx` tag marking an expression-classified filter value.
pub const FILTER_FX_TAG_TEXT_SIZE: f32 = 9.0;
/// Horizontal padding inside the `fx` tag.
pub const FILTER_FX_TAG_PADDING_X: Pixels = px(4.0);
/// Corner radius of the `fx` tag.
pub const FILTER_FX_TAG_RADIUS: f32 = 3.0;

/// Top offset of a filter bar dropdown (the operator menu or the column
/// picker) below the control it opens from.
pub const FILTER_MENU_TOP_OFFSET: Pixels = px(4.0);

/// Width of the operator menu shown while a chip is being edited.
pub const FILTER_OP_MENU_WIDTH: Pixels = px(196.0);
/// Corner radius of the operator menu.
pub const FILTER_OP_MENU_RADIUS: f32 = 6.0;
/// Padding around the operator menu's items.
pub const FILTER_OP_MENU_PADDING: Pixels = px(4.0);
/// Height of one operator menu item.
pub const FILTER_OP_MENU_ITEM_HEIGHT: Pixels = px(26.0);
/// Horizontal padding inside one operator menu item.
pub const FILTER_OP_MENU_ITEM_PADDING_X: Pixels = px(8.0);
/// Corner radius of one operator menu item.
pub const FILTER_OP_MENU_ITEM_RADIUS: f32 = 4.0;
/// Width of an operator menu item's fixed-width symbol column, so every
/// item's trailing pattern hint (where present) lines up in a column.
pub const FILTER_OP_MENU_SYMBOL_WIDTH: Pixels = px(38.0);

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

/// Tallest the modal's connection-row viewport is allowed to grow before it
/// scrolls.
pub const MODAL_LIST_MAX_HEIGHT: Pixels = px(300.0);
/// Width of the connection list's scrollbar track/thumb.
pub const MODAL_LIST_SCROLLBAR_WIDTH: Pixels = px(8.0);
/// Corner radius of the connection list's scrollbar thumb.
pub const MODAL_LIST_SCROLLBAR_RADIUS: f32 = 4.0;
/// Gap between the connection list's scrollbar track and the viewport edge
/// it hugs.
pub const MODAL_LIST_SCROLLBAR_GAP: Pixels = px(4.0);
/// Corner radius of a connection-list row.
pub const MODAL_ROW_RADIUS: f32 = 7.0;
/// Tallest the Open Script picker's row list may grow before it scrolls
/// rather than pushing the modal's footer off screen.
pub const OPEN_MODAL_ROWS_MAX_HEIGHT: Pixels = px(360.0);
/// Background tint marking the currently-connected row in the modal list.
#[must_use]
pub fn modal_row_active_bg(theme: &Theme) -> gpui::Rgba {
    theme.colors.accent_wash_soft()
}
/// Text size of a connection-list row's name.
pub const MODAL_ROW_NAME_TEXT_SIZE: f32 = 12.5;
/// Text size of a connection-list row's url.
pub const MODAL_ROW_URL_TEXT_SIZE: f32 = 10.5;
/// Text size of the "connected" label shown next to the active row's name.
pub const MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE: f32 = 9.5;
/// Vertical gap between a connection-list row's name line and its url line.
pub const MODAL_ROW_INNER_GAP: Pixels = px(3.0);

/// Size of a connection-list row's delete icon.
pub const MODAL_DELETE_ICON_SIZE: Pixels = px(13.0);
/// Size of the "Add connection" affordance's plus icon.
pub const MODAL_ADD_ICON_SIZE: Pixels = px(12.0);

/// Size of a connection-list row's edit (pencil) icon.
pub const MODAL_EDIT_ICON_SIZE: Pixels = px(13.0);

// ---- connection form ---------------------------------------------------

/// Text size of a connection-form field's caption label (e.g. "Host").
pub const CONNECTION_FORM_LABEL_TEXT_SIZE: f32 = 10.0;
/// Vertical gap between a connection-form field's label and its input.
pub const CONNECTION_FORM_LABEL_GAP: Pixels = px(5.0);
/// Vertical gap between successive fields/rows in the connection form.
pub const CONNECTION_FORM_FIELD_GAP: Pixels = px(12.0);
/// Horizontal gap between two fields sharing a row (Host/Port, User/Password).
pub const CONNECTION_FORM_ROW_GAP: Pixels = px(10.0);
/// Fixed width of the Port field, narrower than the Host field beside it.
pub const CONNECTION_FORM_PORT_WIDTH: Pixels = px(96.0);
/// Opacity applied to the driver-field section while the URL does not
/// currently parse, distinct from full removal so the section's shape stays
/// legible as it fades back in once the URL parses again.
pub const CONNECTION_FORM_DIM_OPACITY: f32 = 0.45;
/// Text size of the divider separating the URL from its driver-specific
/// fields, and of the "extra query params" note beneath them.
pub const CONNECTION_FORM_DIVIDER_TEXT_SIZE: f32 = 9.5;
/// Text size of the password field's show/hide toggle and the URL field's
/// detected-driver badge row.
pub const CONNECTION_FORM_TOGGLE_TEXT_SIZE: f32 = 10.5;
/// Text size of the Test button's inline pending/connected/error result.
pub const CONNECTION_FORM_RESULT_TEXT_SIZE: f32 = 12.0;

/// Background wash for the Test button's "connected" result banner.
#[must_use]
pub fn connection_test_ok_bg(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.accent, 0x1f)
}

/// Background wash for the Test button's failure result banner.
#[must_use]
pub fn connection_test_error_bg(theme: &Theme) -> gpui::Rgba {
    theme.colors.error_wash()
}

/// Background wash for the Test button's pending result banner.
#[must_use]
pub fn connection_test_pending_bg(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.status_warn, 0x1f)
}

// ---- results grid value panel ------------------------------------------

/// Height of the value panel's header (column name + pin/expand/close).
pub const VALUE_PANEL_HEADER_HEIGHT: Pixels = px(34.0);
/// Height of the value panel's mode-switcher sub-bar.
pub const VALUE_PANEL_SUBBAR_HEIGHT: Pixels = px(28.0);
/// Height of the value panel's footer (JSON path / parse-failure message).
pub const VALUE_PANEL_FOOTER_HEIGHT: Pixels = px(26.0);
/// Horizontal padding inside the value panel's header/sub-bar/footer/body.
pub const VALUE_PANEL_PADDING_X: Pixels = px(10.0);
/// Text size of the value panel's body content.
pub const VALUE_PANEL_TEXT_SIZE: f32 = 11.5;
/// Text size of the value panel's header/sub-bar/footer labels.
pub const VALUE_PANEL_LABEL_TEXT_SIZE: f32 = 10.5;
/// Height of a mode-switcher/header toggle button.
pub const VALUE_PANEL_BUTTON_HEIGHT: Pixels = px(20.0);
/// Corner radius of a mode-switcher/header toggle button.
pub const VALUE_PANEL_BUTTON_RADIUS: f32 = 5.0;
/// Left indent added per nesting depth in the JSON tree.
pub const VALUE_PANEL_TREE_INDENT: f32 = 14.0;

/// Text color of a disabled mode-switcher button (JSON Tree/Pretty while a
/// json/jsonb cell has failed to parse or has not been fully loaded yet).
#[must_use]
pub fn value_panel_disabled_button_text(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.text_tertiary, 0x80)
}

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
/// The minimum width of the schema sections
pub const SCHEMA_SECTION_WIDTH: Pixels = px(1200.0);

/// Width of a column row's left key-rail tick.
pub const SCHEMA_RAIL_WIDTH: Pixels = px(3.0);
/// Text size of a Keys-cell badge or link chip.
pub const SCHEMA_BADGE_TEXT_SIZE: f32 = 9.5;
/// Horizontal padding inside a Keys-cell badge or link chip.
pub const SCHEMA_BADGE_PADDING_X: Pixels = px(6.0);
/// Corner radius of a Keys-cell badge or link chip.
pub const SCHEMA_BADGE_RADIUS: f32 = 4.0;

/// Border color of the primary-key badge: the accent at a badge-strength
/// alpha distinct from [`zsql_ui::theme::Colors::accent_outline`]'s.
#[must_use]
pub fn schema_badge_pk_border(theme: &Theme) -> gpui::Rgba {
    Colors::wash(theme.colors.accent, 0x52)
}

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
    [px(190.0), px(210.0), px(100.0), px(220.0), px(220.0)];
/// Fixed pixel widths of the Indexes table's cells, in display order (Name,
/// Method, Unique, Definition).
pub const SCHEMA_INDEXES_WIDTHS: [Pixels; 4] = [px(220.0), px(100.0), px(80.0), px(360.0)];
/// Fixed pixel widths of the Constraints table's cells, in display order
/// (Name, Type, Definition).
pub const SCHEMA_CONSTRAINTS_WIDTHS: [Pixels; 3] = [px(260.0), px(140.0), px(360.0)];

// ---- status-bar appearance trigger -------------------------------------

/// Side length of one swatch chip in the status-bar theme trigger.
pub const APPEARANCE_TRIGGER_SWATCH_SIZE: Pixels = px(7.0);
/// Gap between the status-bar theme trigger's swatch chips.
pub const APPEARANCE_TRIGGER_SWATCH_GAP: Pixels = px(2.0);
/// Corner radius of one swatch chip in the status-bar theme trigger.
pub const APPEARANCE_TRIGGER_SWATCH_RADIUS: f32 = 2.0;
/// Horizontal padding inside the status-bar theme trigger.
pub const APPEARANCE_TRIGGER_PADDING_X: Pixels = px(9.0);
/// Vertical padding inside the status-bar theme trigger.
pub const APPEARANCE_TRIGGER_PADDING_Y: Pixels = px(3.0);
/// Corner radius of the status-bar theme trigger.
pub const APPEARANCE_TRIGGER_RADIUS: f32 = 7.0;
/// Horizontal gap between the status-bar theme trigger's swatch, name, and
/// caret.
pub const APPEARANCE_TRIGGER_GAP: Pixels = px(7.0);
/// Text size of the status-bar theme trigger's name and caret.
pub const APPEARANCE_TRIGGER_TEXT_SIZE: f32 = 12.0;

// ---- Appearance modal ---------------------------------------------------

/// Text size of the Appearance modal's title.
pub const APPEARANCE_MODAL_TITLE_TEXT_SIZE: f32 = 15.0;
/// Text size of the Appearance modal's subtitle.
pub const APPEARANCE_MODAL_SUBTITLE_TEXT_SIZE: f32 = 13.0;
/// Tallest the modal's card grid is allowed to grow before it scrolls.
pub const APPEARANCE_MODAL_GRID_MAX_HEIGHT: Pixels = px(520.0);
/// Padding around the modal's scrollable card grid.
pub const APPEARANCE_MODAL_GRID_PADDING: Pixels = px(24.0);
/// Gap between cards in the modal's grid, on both axes.
pub const APPEARANCE_MODAL_GRID_GAP: Pixels = px(20.0);
/// Fixed width of one theme card in the modal's grid.
pub const APPEARANCE_CARD_WIDTH: Pixels = px(262.0);
/// Corner radius of a theme card's mini-preview panel.
pub const APPEARANCE_CARD_PREVIEW_RADIUS: f32 = 10.0;
/// Vertical gap between a card's mini-preview panel and its name/tone row.
pub const APPEARANCE_CARD_META_GAP: Pixels = px(11.0);
/// Text size of a card's theme name.
pub const APPEARANCE_CARD_NAME_TEXT_SIZE: f32 = 13.5;
/// Text size of a card's tone/ACTIVE label.
pub const APPEARANCE_CARD_TONE_TEXT_SIZE: f32 = 9.0;
/// Padding around the modal's footer hint and Done button.
pub const APPEARANCE_FOOTER_PADDING_X: Pixels = px(26.0);
/// Vertical padding around the modal's footer hint and Done button.
pub const APPEARANCE_FOOTER_PADDING_Y: Pixels = px(16.0);
/// Text size of the modal footer's hint text.
pub const APPEARANCE_FOOTER_HINT_TEXT_SIZE: f32 = 12.0;
/// Horizontal padding inside the modal footer's Done button.
pub const APPEARANCE_DONE_BUTTON_PADDING_X: Pixels = px(18.0);
/// Vertical padding inside the modal footer's Done button.
pub const APPEARANCE_DONE_BUTTON_PADDING_Y: Pixels = px(8.0);
/// Corner radius of the modal footer's Done button.
pub const APPEARANCE_DONE_BUTTON_RADIUS: f32 = 7.0;
/// Text size of the modal footer's Done button.
pub const APPEARANCE_DONE_BUTTON_TEXT_SIZE: f32 = 13.0;

// ---- mini zsql preview (painted inside an Appearance-modal card) --------

/// Height of the mini preview's editor line.
pub const MINI_EDITOR_HEIGHT: Pixels = px(30.0);
/// Horizontal padding inside the mini preview's editor line and status
/// strip.
pub const MINI_PADDING_X: Pixels = px(10.0);
/// Text size of the mini preview's editor line and grid.
pub const MINI_TEXT_SIZE: f32 = 10.5;
/// Text size of the mini preview's Run chip.
pub const MINI_RUN_CHIP_TEXT_SIZE: f32 = 9.5;
/// Horizontal padding inside the mini preview's Run chip.
pub const MINI_RUN_CHIP_PADDING_X: Pixels = px(8.0);
/// Corner radius of the mini preview's Run chip.
pub const MINI_RUN_CHIP_RADIUS: f32 = 5.0;
/// Height of one row (header or data) in the mini preview's results grid.
pub const MINI_GRID_ROW_HEIGHT: Pixels = px(24.0);
/// Text size of the mini preview's column type tags.
pub const MINI_TAG_TEXT_SIZE: f32 = 7.5;
/// Horizontal padding inside the mini preview's column type tags.
pub const MINI_TAG_PADDING_X: Pixels = px(4.0);
/// Corner radius of the mini preview's column type tags.
pub const MINI_TAG_RADIUS: f32 = 4.0;
/// Height of the mini preview's status strip.
pub const MINI_STATUS_HEIGHT: Pixels = px(24.0);
/// Text size of the mini preview's status strip.
pub const MINI_STATUS_TEXT_SIZE: f32 = 9.5;

#[cfg(test)]
mod tests {
    use super::{
        GENERATED_STRIP_HEIGHT, GENERATED_STRIP_MAX_LINES, generated_strip_accent,
        generated_strip_bg, generated_strip_height, modal_row_active_bg, run_button_disabled_bg,
        run_button_hint, run_button_hover_bg, schema_badge_pk_border, sidebar_selected_bg,
        status_disconnected,
    };
    use zsql_ui::theme::Theme;

    #[test]
    #[allow(clippy::cast_precision_loss)] // the line cap is a small count
    fn generated_strip_height_grows_per_line_and_caps_at_the_line_limit() {
        let line = gpui::px(zsql_editor::EDITOR_LINE_HEIGHT);
        assert_eq!(generated_strip_height(0), GENERATED_STRIP_HEIGHT);
        assert_eq!(generated_strip_height(1), GENERATED_STRIP_HEIGHT);
        assert_eq!(
            generated_strip_height(3),
            GENERATED_STRIP_HEIGHT + line * 2.0
        );
        assert_eq!(
            generated_strip_height(GENERATED_STRIP_MAX_LINES + 10),
            GENERATED_STRIP_HEIGHT + line * (GENERATED_STRIP_MAX_LINES - 1) as f32,
            "past the cap the strip stops growing and the editor scrolls"
        );
    }

    /// Every app-level derivation in this module must reproduce the exact
    /// ARGB value of the baked constant it replaced, the same way the
    /// underlying `Colors` methods it is built from are pinned in
    /// `zsql-ui`'s own theme tests.
    #[test]
    fn app_level_derivations_reproduce_their_pre_refactor_baked_constants() {
        let theme = Theme::default();

        // was theme::STATUS_DISCONNECTED.
        assert_eq!(status_disconnected(&theme), 0xe2_6d_78);
        // was theme::SIDEBAR_SELECTED_BG.
        assert_eq!(sidebar_selected_bg(&theme), gpui::rgba(0x33_c2_ac_1a));
        // was theme::GENERATED_STRIP_BG.
        assert_eq!(generated_strip_bg(&theme), gpui::rgba(0x33_c2_ac_0b));
        // was theme::GENERATED_STRIP_ACCENT (colors::TEAL_DIM).
        assert_eq!(generated_strip_accent(&theme), 0x2b_85_79);
        // was theme::SCHEMA_BADGE_PK_BORDER.
        assert_eq!(schema_badge_pk_border(&theme), gpui::rgba(0x33_c2_ac_52));
        // was theme::RUN_BUTTON_HOVER_BG.
        assert_eq!(run_button_hover_bg(&theme), 0x46_cf_ba);
        // was theme::RUN_BUTTON_HINT.
        assert_eq!(run_button_hint(&theme), gpui::rgba(0x10_12_17_b3));
        // was theme::MODAL_ROW_ACTIVE_BG.
        assert_eq!(modal_row_active_bg(&theme), gpui::rgba(0x33_c2_ac_17));
    }

    /// The disabled Run button fill must be a dimmer shade of the accent,
    /// not the resting accent itself.
    #[test]
    fn run_button_disabled_bg_is_dimmer_than_the_resting_accent() {
        let theme = Theme::default();
        assert_eq!(run_button_disabled_bg(&theme), theme.colors.accent_dim());
        assert_ne!(run_button_disabled_bg(&theme), theme.colors.accent);
    }
}
