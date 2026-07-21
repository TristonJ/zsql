use gpui::{ClickEvent, Context, Div, IntoElement, Window, div, prelude::*, px, rgb, rgba};
use zsql_ui::button::{primary_button, secondary_button};
use zsql_ui::grid;
use zsql_ui::theme::ActiveTheme;

use super::super::theme;
use super::{ConnectionManagerView, ManagerView, TestOutcome, form, style};

impl ConnectionManagerView {
    /// A field's caption label, in the small uppercase style every field in
    /// the form shares: tertiary color, semibold, letters upper-cased (gpui
    /// has no letter-spacing, so the tracking in the design is dropped).
    pub(super) fn field_label(text: impl Into<String>, colors: zsql_ui::theme::Colors) -> Div {
        div()
            .text_size(px(style::CONNECTION_FORM_LABEL_TEXT_SIZE))
            .text_color(rgb(colors.text_tertiary))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(text.into().to_uppercase())
    }

    /// A labeled field: a caption above the given input entity.
    pub(super) fn labeled_field(
        label: impl Into<String>,
        colors: zsql_ui::theme::Colors,
        field: impl IntoElement,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(style::CONNECTION_FORM_LABEL_GAP)
            .child(Self::field_label(label, colors))
            .child(field)
    }

    /// The add/edit form panel: Name, URL (with its live detected-driver
    /// badge), a divider labeled with the detected driver, the
    /// driver-specific field section (dimmed with an inline reason while
    /// the URL does not parse), the Test result banner, and the footer
    /// buttons.
    pub(super) fn render_modal_form(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let url_is_empty = self.url_field.read(cx).value().is_empty();
        let driver_label = match self.pending_driver_id() {
            Ok(id) => form::driver_display_label(id).to_owned(),
            Err(_) if url_is_empty => String::new(),
            Err(_) => "unrecognized".to_owned(),
        };

        let mut body = div()
            .flex()
            .flex_col()
            .gap(style::CONNECTION_FORM_FIELD_GAP)
            .p_4()
            .child(Self::labeled_field("Name", colors, self.name_field.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(style::CONNECTION_FORM_LABEL_GAP)
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
                                        .text_size(px(style::CONNECTION_FORM_TOGGLE_TEXT_SIZE))
                                        .text_color(rgb(colors.text_secondary))
                                        .child(grid::status_dot(colors.accent))
                                        .child(driver_label.clone()),
                                )
                            }),
                    )
                    .child(self.url_field.clone()),
            );

        body = body.child(self.render_driver_field_section(&driver_label, cx));
        body = body.child(self.render_test_outcome(cx));

        div()
            .flex()
            .flex_col()
            .child(body)
            .child(self.render_form_footer(window, cx))
    }

    /// The divider + driver-specific fields, or a plain hint when the URL's
    /// scheme is not (yet) recognized at all.
    pub(super) fn render_driver_field_section(
        &self,
        driver_label: &str,
        cx: &Context<Self>,
    ) -> Div {
        let colors = cx.theme().colors;
        let Ok(driver_id) = self.pending_driver_id() else {
            return div()
                .text_size(px(style::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child("Enter a URL above to see its fields.");
        };

        let divider = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_size(px(style::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(driver_label.to_uppercase()),
            )
            .child(div().flex_1().h(px(1.0)).bg(rgb(colors.border_soft)));

        let mut section = div()
            .flex()
            .flex_col()
            .gap(style::CONNECTION_FORM_FIELD_GAP)
            .when(self.dim_reason().is_some(), |el| {
                el.opacity(style::CONNECTION_FORM_DIM_OPACITY)
            });

        section = if driver_id == "sqlite" {
            section.child(Self::labeled_field(
                "Database file",
                colors,
                self.sqlite_path_field.clone(),
            ))
        } else {
            self.render_network_fields(section, driver_id, colors, cx)
        };

        if let Some(extras_line) = self.render_extra_query_params_line(driver_id, colors) {
            section = section.child(extras_line);
        }

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(style::CONNECTION_FORM_FIELD_GAP);
        wrapper = wrapper.child(divider).child(section);
        if let Some(reason) = self.dim_reason() {
            wrapper = wrapper.child(
                div()
                    .text_size(px(style::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(reason.to_owned()),
            );
        }
        wrapper
    }

    /// The Host/Port, User/Password, Database, and TLS-param fields shared
    /// by the network drivers (postgres, mssql), appended onto `section`.
    pub(super) fn render_network_fields(
        &self,
        section: Div,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
        cx: &Context<Self>,
    ) -> Div {
        section
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(style::CONNECTION_FORM_ROW_GAP)
                    .child(div().flex_1().child(Self::labeled_field(
                        "Host",
                        colors,
                        self.host_field.clone(),
                    )))
                    .child(
                        div()
                            .w(style::CONNECTION_FORM_PORT_WIDTH)
                            .child(Self::labeled_field("Port", colors, self.port_field.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(style::CONNECTION_FORM_ROW_GAP)
                    .child(div().flex_1().child(Self::labeled_field(
                        "User",
                        colors,
                        self.user_field.clone(),
                    )))
                    .child(div().flex_1().child(self.render_password_field(colors, cx))),
            )
            .child(Self::labeled_field(
                "Database",
                colors,
                self.database_field.clone(),
            ))
            .child(Self::labeled_field(
                form::tls_param_label(driver_id),
                colors,
                self.tls_field.clone(),
            ))
    }

    /// A read-only line listing any query parameters the URL carries beyond
    /// `driver_id`'s own TLS param -- parts a field edit still preserves
    /// (see [`zsql_core::ConnectionUrl::extra_query_params`]) but has no
    /// field of its own to show, so they are surfaced here instead of
    /// silently hidden. `None` for sqlite (no query params) or when there
    /// are none to show.
    pub(super) fn render_extra_query_params_line(
        &self,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
    ) -> Option<Div> {
        if driver_id == "sqlite" {
            return None;
        }
        let parsed = self.parsed_url.as_ref()?;
        let extras = parsed.extra_query_params(&[form::tls_param_key(driver_id)]);
        if extras.is_empty() {
            return None;
        }
        let text = extras
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(
            div()
                .text_size(px(style::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child(format!("extra params (edit in the URL above): {text}")),
        )
    }

    /// The password field with its trailing show/hide toggle.
    pub(super) fn render_password_field(
        &self,
        colors: zsql_ui::theme::Colors,
        cx: &Context<Self>,
    ) -> Div {
        let masked = self.password_field.read(cx).is_masked();
        div()
            .flex()
            .flex_col()
            .gap(style::CONNECTION_FORM_LABEL_GAP)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(Self::field_label("Password", colors))
                    .child(
                        div()
                            .id("connection-form-password-toggle")
                            .ml_auto()
                            .cursor_pointer()
                            .text_size(px(style::CONNECTION_FORM_TOGGLE_TEXT_SIZE))
                            .text_color(rgb(colors.text_tertiary))
                            .child(if masked { "show" } else { "hide" })
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.toggle_password_visible(cx);
                            })),
                    ),
            )
            .child(self.password_field.clone())
    }

    /// The inline Test-button result banner: nothing, a pending indicator,
    /// a connected-with-elapsed-ms line, or the driver's verbatim error.
    pub(super) fn render_test_outcome(&self, cx: &Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let Some(outcome) = self.test_outcome() else {
            return div();
        };
        let (bg, dot_color, text) = match outcome {
            TestOutcome::Pending => (
                style::connection_test_pending_bg(active_theme),
                colors.status_warn,
                "Testing...".to_owned(),
            ),
            TestOutcome::Connected { elapsed_ms } => (
                style::connection_test_ok_bg(active_theme),
                colors.accent,
                format!("Connected - {elapsed_ms} ms"),
            ),
            TestOutcome::Failed(message) => (
                style::connection_test_error_bg(active_theme),
                colors.status_error,
                message.clone(),
            ),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(px(style::MODAL_ROW_RADIUS))
            .bg(rgba(bg))
            .text_size(px(style::CONNECTION_FORM_RESULT_TEXT_SIZE))
            .text_color(rgb(dot_color))
            .overflow_hidden()
            .child(div().min_w_0().child(text))
    }

    /// The form's footer: Cancel, Test, and (add form) Connect + Save, or
    /// (edit form) Save changes only.
    pub(super) fn render_form_footer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;

        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(rgb(colors.border_soft))
            .child(
                secondary_button("connection-form-cancel", window, cx)
                    .track_focus(&self.cancel_focus)
                    .child("Cancel")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.cancel_form(cx);
                    })),
            )
            .child(
                secondary_button("connection-form-test", window, cx)
                    .track_focus(&self.test_focus)
                    .child("Test")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.run_test(cx).detach();
                    })),
            )
            .child(div().flex_1());

        match self.current_view() {
            ManagerView::AddForm => {
                footer = footer
                    .child(
                        secondary_button("connection-form-connect", window, cx)
                            .track_focus(&self.connect_focus)
                            .child("Connect")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.connect_unsaved(cx).detach();
                            })),
                    )
                    .child(
                        primary_button("connection-form-save", window, cx)
                            .track_focus(&self.save_focus)
                            .child("Save")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                let _ = view.add_connection(cx);
                            })),
                    );
            }
            ManagerView::EditForm { index } => {
                footer = footer.child(
                    primary_button("connection-form-save", window, cx)
                        .track_focus(&self.save_focus)
                        .child("Save changes")
                        .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                            let _ = view.save_edit(index, cx);
                        })),
                );
            }
            ManagerView::List => {}
        }

        footer
    }

    pub(super) fn render_status(&self, cx: &Context<Self>) -> Div {
        let text = self
            .status()
            .unwrap_or("click a row to connect\t•\tesc to close");
        div()
            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
            .text_color(rgb(cx.theme().colors.text_tertiary))
            .child(text.to_owned())
    }
}
