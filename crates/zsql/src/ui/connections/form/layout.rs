//! Whether the connection form lays out as one column or two, and the
//! two-column body itself: base connection fields on the left, the SSH
//! tunnel section on the right. Off, or for a non-network driver, the form
//! stays the single narrow column [`super::ConnectionForm::render`] already
//! builds; the SSH section only ever moves into its own column once it is
//! both enabled and actually mounted.

use gpui::{Context, Div, Window, div, prelude::*, px, rgb};
use zsql_ui::{grid, modal::ModalSize, theme::ActiveTheme};

use crate::drivers::is_network;
use crate::ui::theme;

use super::ConnectionForm;

/// How many columns the form currently renders as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormColumns {
    /// Base connection fields only, with the SSH section (if the driver
    /// mounts it at all) collapsed to a trailing row.
    Single,
    /// Base connection fields on the left, the SSH tunnel section on the
    /// right.
    Two,
}

/// The pure branch behind [`ConnectionForm::form_columns`]: two columns
/// only once the SSH section is both enabled and actually mounted for the
/// current driver, matching the same gate [`ConnectionForm::ssh_state`]
/// uses so the layout and the SSH state it carries never disagree.
fn form_columns(ssh_enabled: bool, is_network_driver: bool) -> FormColumns {
    if ssh_enabled && is_network_driver {
        FormColumns::Two
    } else {
        FormColumns::Single
    }
}

impl ConnectionForm {
    /// How many columns the form currently renders as -- see
    /// [`form_columns`].
    pub(crate) fn form_columns(&self) -> FormColumns {
        let is_network_driver = self.pending_driver_id().is_ok_and(is_network);
        form_columns(self.ssh_enabled, is_network_driver)
    }

    /// The modal panel width this form's current layout needs: the
    /// standard narrow width for a single column, or a wider fixed width
    /// for two. There is no in-between -- a connection has exactly one
    /// transport, so the form never needs more than two columns.
    #[must_use]
    pub fn modal_size(&self) -> ModalSize {
        match self.form_columns() {
            FormColumns::Single => ModalSize::Small,
            FormColumns::Two => ModalSize::Wide,
        }
    }

    /// The URL field's caption row (with the live detected-driver badge)
    /// plus the field itself -- shared by the single- and two-column
    /// layouts.
    pub(super) fn render_url_field(
        &self,
        driver_label: &str,
        colors: zsql_ui::theme::Colors,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(Self::field_label("URL", colors))
                    .when(!driver_label.is_empty(), |el| {
                        el.child(
                            div()
                                .ml_auto()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .text_size(px(theme::CONNECTION_FORM_TOGGLE_TEXT_SIZE))
                                .text_color(rgb(colors.text_secondary))
                                .child(grid::status_dot(colors.accent))
                                .child(driver_label.to_owned()),
                        )
                    }),
            )
            .child(self.url_field.clone())
    }

    /// The form's single-column body: Name, URL, the driver-specific field
    /// section (with the SSH tunnel collapsed to a trailing row while it is
    /// off, or absent for a non-network driver), and the test-outcome
    /// banner. Used whenever [`Self::form_columns`] selects
    /// [`FormColumns::Single`].
    pub(super) fn render_single_column_body(
        &self,
        driver_label: &str,
        colors: zsql_ui::theme::Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .p_4()
            .child(Self::labeled_field("Name", colors, self.name_field.clone()))
            .child(self.render_url_field(driver_label, colors))
            .child(self.render_driver_field_section(driver_label, window, cx))
            .child(self.render_test_outcome(cx))
    }

    /// The two-column body: Name, URL, and the driver-specific fields (SSH
    /// excluded) on the left; the SSH tunnel section, its own enable toggle
    /// in its header, on the right. The test-outcome banner spans the full
    /// width below both columns. Used whenever [`Self::form_columns`]
    /// selects [`FormColumns::Two`]; the footer is added by [`Self::render`]
    /// itself so it always spans the whole panel, in either layout.
    pub(super) fn render_two_column_body(
        &self,
        driver_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let colors = cx.theme().colors;

        let left = div()
            .flex_1()
            .min_w_0()
            .p_4()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .child(Self::labeled_field("Name", colors, self.name_field.clone()))
            .child(self.render_url_field(driver_label, colors))
            .child(self.render_driver_field_section(driver_label, window, cx));

        let right = div()
            .flex_1()
            .min_w_0()
            .p_4()
            .border_l_1()
            .border_color(rgb(colors.border))
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .child(self.render_ssh_section(colors, window, cx));

        // Flexbox's default `align-items: stretch` is exactly what the two
        // columns need -- the shorter one's border/background still reaches
        // the taller one's full height with no extra styling.
        let columns = div().flex().flex_row().child(left).child(right);

        div()
            .flex()
            .flex_col()
            .child(columns)
            .child(div().px_4().pb_4().child(self.render_test_outcome(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::{FormColumns, form_columns};

    #[test]
    fn two_columns_only_once_ssh_is_enabled_and_the_driver_mounts_the_section() {
        assert_eq!(
            form_columns(false, true),
            FormColumns::Single,
            "ssh off stays single-column even for a network driver"
        );
        assert_eq!(
            form_columns(true, false),
            FormColumns::Single,
            "ssh on with no network driver (the section unmounted) stays single-column"
        );
        assert_eq!(form_columns(false, false), FormColumns::Single);
        assert_eq!(
            form_columns(true, true),
            FormColumns::Two,
            "ssh on for a network driver opens the second column"
        );
    }
}
