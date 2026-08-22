//! The password prompt shown when a connection's keyring secret is absent
//! but its sanitized (password-cleared) URL survives: a [`ModalSize::Small`]
//! modal over a connection summary, a masked password field, and a "save to
//! system keyring" checkbox. [`PasswordPrompt`] owns only its own input
//! state; it never touches the session or the keyring itself, emitting
//! [`PasswordPromptEvent`] for its opener to act on instead.

use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Render, Window, div, prelude::*, px, rgb,
};
use zsql_ui::{
    button::secondary_button,
    grid,
    modal::{Modal, ModalSize},
    text_field::{TextFieldEvent, TextFieldState},
    theme::ActiveTheme,
};

use crate::connections::StoredConnection;
use crate::ui::format::connection_summary_line;
use crate::ui::theme;

/// The inline error shown for a blank password, on submit.
pub(super) const EMPTY_PASSWORD_MESSAGE: &str = "Enter a password to connect.";

/// What [`PasswordPrompt`] asks its opener to do, in response to a footer
/// button click, Escape, or Enter in the password field.
pub(super) enum PasswordPromptEvent {
    /// The user cancelled: discard the typed password without connecting.
    Cancel,
    /// Attempt the connection with `password`, writing it to the keyring
    /// afterward only if `save_to_keyring` is true and the attempt succeeds.
    Connect {
        password: String,
        save_to_keyring: bool,
    },
}

/// The password prompt's state: which connection it is for, its
/// password-cleared URL to rebuild from, the password field, and the "save
/// to keyring" checkbox. Its opener drives [`Self::set_error`] to reflect a
/// failed connect attempt back into the prompt.
pub(super) struct PasswordPrompt {
    connection: StoredConnection,
    sanitized_url: String,
    password_field: Entity<TextFieldState>,
    save_to_keyring: bool,
    error: Option<String>,
    connecting: bool,
    focus_handle: FocusHandle,
    /// Whether the password field still needs focusing on the next render,
    /// deferred because opening the prompt has no [`Window`] to focus with
    /// synchronously.
    needs_focus: bool,
}

impl EventEmitter<PasswordPromptEvent> for PasswordPrompt {}

impl PasswordPrompt {
    pub(super) fn new(
        connection: StoredConnection,
        sanitized_url: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let password_field = cx.new(|cx| {
            let mut field = TextFieldState::new("password", None, cx);
            field.set_masked(true, cx);
            field
        });
        cx.subscribe(&password_field, |prompt, _field, event, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                prompt.submit(cx);
            }
        })
        .detach();

        Self {
            connection,
            sanitized_url,
            password_field,
            save_to_keyring: true,
            error: None,
            connecting: false,
            focus_handle: cx.focus_handle(),
            needs_focus: true,
        }
    }

    pub(super) fn connection(&self) -> &StoredConnection {
        &self.connection
    }

    pub(super) fn sanitized_url(&self) -> &str {
        &self.sanitized_url
    }

    pub(super) fn toggle_save(&mut self, cx: &mut Context<Self>) {
        self.save_to_keyring = !self.save_to_keyring;
        cx.notify();
    }

    /// Discard the typed password without emitting a connect attempt.
    pub(super) fn cancel(cx: &mut Context<Self>) {
        cx.emit(PasswordPromptEvent::Cancel);
    }

    /// A blank password is rejected locally, without ever reaching the
    /// opener; a non-blank one is handed off via
    /// [`PasswordPromptEvent::Connect`], marking the prompt as connecting
    /// until the opener reports back through [`Self::set_error`].
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        let password = self.password_field.read(cx).value().to_string();
        if password.trim().is_empty() {
            self.set_error(Some(EMPTY_PASSWORD_MESSAGE.to_owned()), cx);
            return;
        }
        self.connecting = true;
        self.error = None;
        cx.notify();
        cx.emit(PasswordPromptEvent::Connect {
            password,
            save_to_keyring: self.save_to_keyring,
        });
    }

    pub(super) fn set_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.connecting = false;
        self.error = error;
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub(super) fn save_checked(&self) -> bool {
        self.save_to_keyring
    }

    #[cfg(test)]
    pub(super) fn connecting(&self) -> bool {
        self.connecting
    }

    #[cfg(test)]
    pub(super) fn field_is_masked(&self, cx: &App) -> bool {
        self.password_field.read(cx).is_masked()
    }

    #[cfg(test)]
    pub(super) fn field_focus_handle(&self, cx: &App) -> FocusHandle {
        self.password_field.read(cx).focus_handle(cx)
    }

    #[cfg(test)]
    pub(super) fn password_field(&self) -> Entity<TextFieldState> {
        self.password_field.clone()
    }
}

impl PasswordPrompt {
    /// The modal head: eyebrow and title.
    fn render_head(&self, cx: &App) -> Div {
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_col()
            .justify_center()
            .gap(theme::PASSWORD_PROMPT_HEAD_GAP)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_1()
                    .text_size(px(theme::PASSWORD_PROMPT_EYEBROW_TEXT_SIZE))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(colors.accent))
                            .child("CONNECTION"),
                    )
                    .child(
                        div()
                            .text_color(rgb(colors.text_tertiary))
                            .child(format!("\u{b7} {}", self.connection.name)),
                    ),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(colors.text_primary))
                    .child("Password required"),
            )
    }

    fn render_intro(cx: &App) -> Div {
        let colors = cx.theme().colors;
        div()
            .text_size(px(theme::PASSWORD_PROMPT_SUBTITLE_TEXT_SIZE))
            .text_color(rgb(colors.text_secondary))
            .child("No password was found in your system keyring for this connection.")
    }

    fn render_connection_card(&self, cx: &App) -> Div {
        let colors = cx.theme().colors;
        let active_theme = cx.theme();
        let summary_line = connection_summary_line(
            Some(self.sanitized_url.as_str()),
            &self.connection.display_host,
        );
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme::PASSWORD_PROMPT_CARD_GAP)
            .h(theme::PASSWORD_PROMPT_CARD_HEIGHT)
            .px_3()
            .bg(rgb(colors.bg_app))
            .border_1()
            .border_color(rgb(colors.border_soft))
            .rounded(px(theme::PASSWORD_PROMPT_CARD_RADIUS))
            .child(grid::type_tag_accent(
                &self.connection.display_kind,
                active_theme,
            ))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(colors.text_primary))
                    .child(self.connection.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .font_family(active_theme.fonts.data.clone())
                    .text_color(rgb(colors.text_tertiary))
                    .child(summary_line),
            )
    }

    fn render_password_field(&self, cx: &App) -> Div {
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(
                div()
                    .text_size(px(theme::CONNECTION_FORM_LABEL_TEXT_SIZE))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(colors.text_tertiary))
                    .child("PASSWORD"),
            )
            .child(self.password_field.clone())
            .when_some(self.error.clone(), |el, message| {
                el.child(
                    div()
                        .text_size(px(theme::PASSWORD_PROMPT_HINT_TEXT_SIZE))
                        .text_color(rgb(colors.status_error))
                        .child(message),
                )
            })
    }

    fn render_save_checkbox(&self, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let checkbox = if self.save_to_keyring {
            div()
                .bg(rgb(colors.accent))
                .text_color(rgb(colors.bg_app))
                .child("\u{2713}")
        } else {
            div().border_1().border_color(rgb(colors.border))
        };
        let checkbox = checkbox
            .flex_shrink_0()
            .w(theme::PASSWORD_PROMPT_CHECKBOX_SIZE)
            .h(theme::PASSWORD_PROMPT_CHECKBOX_SIZE)
            .rounded(px(theme::PASSWORD_PROMPT_CHECKBOX_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(theme::PASSWORD_PROMPT_HINT_TEXT_SIZE));

        let hint = if self.save_to_keyring {
            "Stored under the same keyring entry the connection form writes."
        } else {
            "Used for this session only; the keyring is left untouched and this prompt \
             returns next time."
        };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id("password-prompt-save-checkbox")
                    .debug_selector(|| "password-prompt-save-checkbox".to_owned())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .text_size(px(theme::PASSWORD_PROMPT_SUBTITLE_TEXT_SIZE))
                    .text_color(rgb(colors.text_secondary))
                    .child(checkbox)
                    .child("Save to system keyring")
                    .on_click(cx.listener(|prompt, _e: &ClickEvent, _w, cx| {
                        prompt.toggle_save(cx);
                    })),
            )
            .child(
                div()
                    .pl(theme::PASSWORD_PROMPT_CHECKBOX_SIZE + theme::PASSWORD_PROMPT_CARD_GAP)
                    .text_size(px(theme::PASSWORD_PROMPT_HINT_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(hint),
            )
    }

    /// A bordered key chip (e.g. "Enter") followed by what it does (e.g.
    /// "connect").
    fn render_hint_chip(key: &'static str, action: &'static str, cx: &App) -> Div {
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .font_family(cx.theme().fonts.data.clone())
                    .text_size(px(theme::PASSWORD_PROMPT_KEY_CHIP_TEXT_SIZE))
                    .text_color(rgb(colors.text_secondary))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .rounded(px(theme::PASSWORD_PROMPT_KEY_CHIP_RADIUS))
                    .px(px(theme::PASSWORD_PROMPT_KEY_CHIP_PADDING_X))
                    .child(key),
            )
            .child(action)
    }

    fn render_footer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let connect_bg = if self.connecting {
            theme::run_button_disabled_bg(active_theme)
        } else {
            colors.accent
        };
        let connect_hover_bg = theme::run_button_hover_bg(active_theme);
        let connecting = self.connecting;

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_4()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(rgb(colors.border_soft))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .text_size(px(theme::PASSWORD_PROMPT_HINT_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(Self::render_hint_chip("Enter", "connect", cx))
                    .child(Self::render_hint_chip("Esc", "cancel", cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        secondary_button("password-prompt-cancel", window, cx)
                            .debug_selector(|| "password-prompt-cancel".to_owned())
                            .child("Cancel")
                            .on_click(cx.listener(|_prompt, _e: &ClickEvent, _w, cx| {
                                Self::cancel(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("password-prompt-connect")
                            .debug_selector(|| "password-prompt-connect".to_owned())
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(theme::RUN_BUTTON_HEIGHT)
                            .px(px(theme::RUN_BUTTON_PADDING_X))
                            .rounded(px(theme::RUN_BUTTON_RADIUS))
                            .bg(rgb(connect_bg))
                            .text_size(px(theme::RUN_BUTTON_TEXT_SIZE))
                            .text_color(rgb(colors.bg_app))
                            .when(!connecting, |el| {
                                el.cursor_pointer()
                                    .hover(|style| style.bg(rgb(connect_hover_bg)))
                                    .on_click(cx.listener(|prompt, _e: &ClickEvent, _w, cx| {
                                        prompt.submit(cx);
                                    }))
                            })
                            .when(connecting, gpui::Styled::cursor_not_allowed)
                            .child(if connecting {
                                "Connecting..."
                            } else {
                                "Connect"
                            }),
                    ),
            )
    }
}

impl Render for PasswordPrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.needs_focus) {
            self.password_field.read(cx).focus_handle(cx).focus(window);
        }

        let body = div()
            .flex()
            .flex_col()
            .gap(theme::PASSWORD_PROMPT_BODY_GAP)
            .px_3()
            .py_3()
            .child(Self::render_intro(cx))
            .child(self.render_connection_card(cx))
            .child(self.render_password_field(cx))
            .child(self.render_save_checkbox(cx));
        let footer = self.render_footer(window, cx);
        let head = self.render_head(cx);
        let focus_handle = self.focus_handle.clone();

        Modal::<Div, Div>::new("password-prompt-modal")
            .size(ModalSize::Small)
            .track_focus(&focus_handle)
            .on_close(cx.listener(|_prompt, (), _w, cx| Self::cancel(cx)))
            .head(head)
            .body(div().flex().flex_col().child(body).child(footer))
    }
}

#[cfg(test)]
mod tests;
