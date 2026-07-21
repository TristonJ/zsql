use gpui::Pixels;
use gpui::px;
use zsql_ui::theme::{Colors, Theme};

/// Width of the centered connection-manager modal panel.
pub(super) const MODAL_WIDTH: Pixels = px(468.0);
/// Corner radius of the modal panel.
pub(super) const MODAL_RADIUS: f32 = 10.0;
/// Height of the modal's title bar.
pub(super) const MODAL_HEAD_HEIGHT: Pixels = px(44.0);
/// Tallest the modal's connection list is allowed to grow before it scrolls.
pub(super) const MODAL_LIST_MAX_HEIGHT: Pixels = px(300.0);
/// Corner radius of a connection-list row.
pub(super) const MODAL_ROW_RADIUS: f32 = 7.0;
/// Background tint marking the currently-connected row in the modal list.
#[must_use]
pub(super) fn modal_row_active_bg(theme: &Theme) -> u32 {
    theme.colors.accent_wash_soft()
}
/// Text size of a connection-list row's name.
pub(super) const MODAL_ROW_NAME_TEXT_SIZE: f32 = 12.5;
/// Text size of a connection-list row's url.
pub(super) const MODAL_ROW_URL_TEXT_SIZE: f32 = 10.5;
/// Text size of the "connected" label shown next to the active row's name.
pub(super) const MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE: f32 = 9.5;
/// Vertical gap between a connection-list row's name line and its url line.
pub(super) const MODAL_ROW_INNER_GAP: Pixels = px(3.0);

/// Size of the modal head's close icon.
pub(super) const MODAL_CLOSE_ICON_SIZE: Pixels = px(13.0);
/// Size of a connection-list row's delete icon.
pub(super) const MODAL_DELETE_ICON_SIZE: Pixels = px(13.0);
/// Size of the "Add connection" affordance's plus icon.
pub(super) const MODAL_ADD_ICON_SIZE: Pixels = px(12.0);

/// `group()` name tying the modal close row's hitbox to its icon's
/// `group_hover` tint, so hovering anywhere in the row -- not just the
/// icon's own small hitbox -- lightens the close glyph.
pub(super) const MODAL_CLOSE_HOVER_GROUP: &str = "connection-modal-close-hover";
/// Size of a connection-list row's edit (pencil) icon.
pub(super) const MODAL_EDIT_ICON_SIZE: Pixels = px(13.0);

/// Text size of a connection-form field's caption label (e.g. "Host").
pub(super) const CONNECTION_FORM_LABEL_TEXT_SIZE: f32 = 10.0;
/// Vertical gap between a connection-form field's label and its input.
pub(super) const CONNECTION_FORM_LABEL_GAP: Pixels = px(5.0);
/// Vertical gap between successive fields/rows in the connection form.
pub(super) const CONNECTION_FORM_FIELD_GAP: Pixels = px(12.0);
/// Horizontal gap between two fields sharing a row (Host/Port, User/Password).
pub(super) const CONNECTION_FORM_ROW_GAP: Pixels = px(10.0);
/// Fixed width of the Port field, narrower than the Host field beside it.
pub(super) const CONNECTION_FORM_PORT_WIDTH: Pixels = px(96.0);
/// Opacity applied to the driver-field section while the URL does not
/// currently parse, distinct from full removal so the section's shape stays
/// legible as it fades back in once the URL parses again.
pub(super) const CONNECTION_FORM_DIM_OPACITY: f32 = 0.45;
/// Text size of the divider separating the URL from its driver-specific
/// fields, and of the "extra query params" note beneath them.
pub(super) const CONNECTION_FORM_DIVIDER_TEXT_SIZE: f32 = 9.5;
/// Text size of the password field's show/hide toggle and the URL field's
/// detected-driver badge row.
pub(super) const CONNECTION_FORM_TOGGLE_TEXT_SIZE: f32 = 10.5;
/// Text size of the Test button's inline pending/connected/error result.
pub(super) const CONNECTION_FORM_RESULT_TEXT_SIZE: f32 = 12.0;

/// Background wash for the Test button's "connected" result banner.
#[must_use]
pub(super) fn connection_test_ok_bg(theme: &Theme) -> u32 {
    Colors::wash(theme.colors.accent, 0x1f)
}

/// Background wash for the Test button's failure result banner.
#[must_use]
pub(super) fn connection_test_error_bg(theme: &Theme) -> u32 {
    theme.colors.error_wash()
}

/// Background wash for the Test button's pending result banner.
#[must_use]
pub(super) fn connection_test_pending_bg(theme: &Theme) -> u32 {
    Colors::wash(theme.colors.status_warn, 0x1f)
}

#[cfg(test)]
mod tests {
    use zsql_ui::theme::Theme;

    use super::modal_row_active_bg;

    /// The derivation must reproduce the exact ARGB value of the baked
    /// constant it replaced, the same way the app-level derivations in
    /// `ui::theme` are pinned.
    #[test]
    fn modal_row_active_bg_reproduces_its_pre_refactor_baked_constant() {
        assert_eq!(modal_row_active_bg(&Theme::default()), 0x33_c2_ac_17);
    }
}
