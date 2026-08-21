//! The Save Script / Save as / Rename modal: a name field with a fixed
//! `.sql` suffix, destination radio rows (Save and Save-as only -- Rename
//! shows just the name field and path preview), and a live path preview,
//! built from `zsql_ui::Modal` (`ModalSize::Small`) and `zsql_ui`'s text
//! field, the same construction idiom as the connection-manager modal (see
//! `crate::ui::connections::form`).

mod bindings;
mod logic;

use std::path::PathBuf;

pub(crate) use bindings::SaveModalBindings;
use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    Render, SharedString, Window, actions, div, prelude::*, px, rgb, rgba,
};
pub use logic::{Destination, NameError, SQL_SUFFIX, validate_for_save};
use zsql_ui::button::{primary_button, secondary_button};
use zsql_ui::modal::{Modal, ModalSize};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState, TextFieldStyle};
use zsql_ui::theme::ActiveTheme;

use super::tabs::TabId;
use crate::ui::theme;

/// The key context [`SaveModalView`]'s destination Up/Down navigation is
/// scoped to. An ancestor of the name field's own `TextField` key context in
/// the render tree, the same pattern `open_modal::KEY_CONTEXT` uses for its
/// row navigation: up/down resolve here since the field itself binds
/// neither, while every key the field does bind (including digits --
/// `top-10`, `q3-report`, `report-2024` type exactly as written) keeps
/// working normally.
pub const KEY_CONTEXT: &str = "SaveModal";

actions!(
    save_modal,
    [SelectPreviousDestination, SelectNextDestination]
);

/// Register this modal's destination Up/Down navigation key bindings from
/// `bindings`. Call once at startup, before any window that hosts a
/// [`SaveModalView`] is opened.
pub fn init(cx: &mut App, bindings: &SaveModalBindings) {
    let mut keys = Vec::new();
    crate::keybindings::bind_all(
        &mut keys,
        &bindings.select_previous_destination,
        &SelectPreviousDestination,
        KEY_CONTEXT,
    );
    crate::keybindings::bind_all(
        &mut keys,
        &bindings.select_next_destination,
        &SelectNextDestination,
        KEY_CONTEXT,
    );
    let registered = keys.len();
    cx.bind_keys(keys);
    tracing::debug!(registered, "save modal keybindings registered");
}

/// What the modal is doing. Save and Save-as both show the destination rows
/// (Save-as always starts on the tab's current destination, if it is
/// library-backed, else `Connection`); Rename shows only the name field and
/// path preview, fixed to whichever destination the tab already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveModalKind {
    Save,
    SaveAs,
    Rename,
}

/// Whether `kind` shows destination rows, and therefore whether the modal
/// binds Up/Down destination navigation. Rename has no destination to
/// navigate.
fn destination_shortcuts_active(kind: SaveModalKind) -> bool {
    matches!(kind, SaveModalKind::Save | SaveModalKind::SaveAs)
}

/// The stable `id()` suffix for `destination`'s row, independent of its
/// display label (which is free to change without breaking a debug
/// selector or test).
fn destination_slug(destination: Destination) -> &'static str {
    match destination {
        Destination::Connection => "connection",
        Destination::Library => "library",
        Destination::External => "external",
    }
}

/// The tab and destination context a modal session was opened for, and the
/// filesystem locations its live validation checks against.
struct OpenState {
    tab_id: TabId,
    kind: SaveModalKind,
    /// The active connection's display name, shown next to "This
    /// connection" so the destination reads as a concrete place.
    connection_name: String,
    session_dir: PathBuf,
    library_dir: PathBuf,
    /// `Rename`'s own current file, excluded from the duplicate-name check
    /// so the modal never opens already showing an error against the exact
    /// file it is renaming. Set by the caller from the tab's actual
    /// [`crate::session_store::ScriptBacking`] (see
    /// `ui::workspace::WorkspaceView::open_rename_modal`), never derived
    /// from `initial_name`/`initial_destination` here -- those seed the
    /// name field and destination for `Save`/`Save-as` too, where they
    /// never name a real "current file" to exclude: `Save-as` always
    /// exports a copy, so a coincidental match with an existing file is a
    /// genuine conflict, not a self-reference. Always `None` outside
    /// `Rename`.
    current_path: Option<PathBuf>,
    error: Option<NameError>,
}

/// What [`SaveModalView`] asks its parent (`ui::workspace::WorkspaceView`,
/// the only entity with the session store and library directory this
/// needs) to do.
#[derive(Debug, Clone)]
pub enum SaveModalEvent {
    /// The user confirmed a valid name+destination.
    Confirmed {
        tab_id: TabId,
        kind: SaveModalKind,
        name: String,
        destination: Destination,
    },
    /// The user cancelled (Escape, the close icon, or the Cancel button).
    /// No file was written and the tab is unchanged.
    Cancelled,
}

impl EventEmitter<SaveModalEvent> for SaveModalView {}

/// The Save Script / Save as / Rename modal's state: whether it is open (and
/// for which tab/mode), the name field, the selected destination, and the
/// live validation error (if any).
pub struct SaveModalView {
    open: Option<OpenState>,
    name_field: Entity<TextFieldState>,
    destination: Destination,
    modal_focus: FocusHandle,
    save_focus: FocusHandle,
    cancel_focus: FocusHandle,
    /// Set by [`Self::open`], consumed by the next `render`: focuses the
    /// name field so keystrokes reach it immediately instead of whatever
    /// held focus before the modal opened (mirrors
    /// `ConnectionManagerView::refocus_modal`).
    refocus_name_field: bool,
    /// The name [`Self::revalidate`] last validated, so the field observer
    /// can tell a real edit apart from a caret-blink notify.
    last_validated_name: String,
}

impl SaveModalView {
    #[must_use]
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Borderless: `render_name_row`'s wrapper draws the border around
        // the field and its `.sql` suffix together, so the suffix reads as
        // part of the field.
        let name_field = cx.new(|cx| {
            TextFieldState::new("name", None, cx).style(TextFieldStyle {
                border_w: px(0.0),
                ..TextFieldStyle::default()
            })
        });
        cx.subscribe(&name_field, |view, _field, event, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                view.confirm(cx);
            }
        })
        .detach();
        // Revalidate only when the name actually changed: the field
        // notifies for every visual change too (its caret blink loop ticks
        // roughly every half second while focused), and revalidating stats
        // the destination file each time.
        cx.observe(&name_field, |view, field, cx| {
            if field.read(cx).value().as_ref() != view.last_validated_name {
                view.revalidate(cx);
            }
        })
        .detach();

        Self {
            open: None,
            name_field,
            destination: Destination::Connection,
            modal_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle(),
            refocus_name_field: false,
            last_validated_name: String::new(),
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Open the modal for `tab_id`, seeding the name field with
    /// `initial_name` (empty for a brand-new unnamed tab) and the
    /// destination with `initial_destination`. `current_path` is `Rename`'s
    /// own current file (see [`OpenState::current_path`]'s doc for why it
    /// is the caller's job to resolve, never derived here from
    /// `initial_name`/`initial_destination`); pass `None` for `Save`/
    /// `Save-as`, which never have one.
    // Every parameter is an independent, already-resolved piece of state
    // the modal needs to seed and validate against; grouping them into a
    // wrapper struct would only move the field list, not shrink it (see
    // `WorkspaceView::new`'s identical justification).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        tab_id: TabId,
        kind: SaveModalKind,
        initial_name: &str,
        initial_destination: Destination,
        connection_name: String,
        session_dir: PathBuf,
        library_dir: PathBuf,
        current_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.name_field
            .update(cx, |field, cx| field.set_value(initial_name, cx));
        self.destination = initial_destination;
        self.open = Some(OpenState {
            tab_id,
            kind,
            connection_name,
            session_dir,
            library_dir,
            current_path,
            error: None,
        });
        self.refocus_name_field = true;
        self.revalidate(cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.emit(SaveModalEvent::Cancelled);
            cx.notify();
        }
    }

    /// A no-op outside of `Save`/`SaveAs` (no modal open, or `Rename`, whose
    /// destination is fixed to the tab's own backing and never rendered as
    /// switchable rows): `Rename` must not let a stray Up/Down keystroke
    /// revalidate the name against the wrong directory and mask a real
    /// same-name collision in the tab's actual destination.
    fn set_destination(&mut self, destination: Destination, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        if !matches!(open.kind, SaveModalKind::Save | SaveModalKind::SaveAs) {
            return;
        }
        self.destination = destination;
        self.revalidate(cx);
        cx.notify();
    }

    fn revalidate(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &mut self.open else {
            return;
        };
        let name = self.name_field.read(cx).value().to_string();
        self.last_validated_name.clone_from(&name);
        open.error = validate_for_save(
            &name,
            self.destination,
            &open.session_dir,
            &open.library_dir,
            open.current_path.as_deref(),
        )
        .err();
    }

    #[must_use]
    fn can_save(&self) -> bool {
        self.open.as_ref().is_some_and(|open| open.error.is_none())
    }

    /// Confirm the current name+destination. Re-validates (a fresh `is_file`
    /// stat) rather than trusting [`Self::can_save`]'s last-keystroke cache:
    /// a file can land at the destination path between the last keystroke
    /// and pressing Enter/Save (e.g. this app's own detached background
    /// library writers, or a second instance). On a fresh validation
    /// failure, the modal stays open with `open` restored and the new error
    /// shown, exactly like the keystroke-time error path -- never a panic.
    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_save() {
            return;
        }
        let Some(mut open) = self.open.take() else {
            return;
        };
        let name = self.name_field.read(cx).value().to_string();
        match validate_for_save(
            &name,
            self.destination,
            &open.session_dir,
            &open.library_dir,
            open.current_path.as_deref(),
        ) {
            Ok(normalized) => {
                cx.emit(SaveModalEvent::Confirmed {
                    tab_id: open.tab_id,
                    kind: open.kind,
                    name: normalized,
                    destination: self.destination,
                });
            }
            Err(err) => {
                open.error = Some(err);
                self.open = Some(open);
            }
        }
        cx.notify();
    }

    fn select_previous_destination(
        &mut self,
        _: &SelectPreviousDestination,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_destination(false, cx);
    }

    fn select_next_destination(
        &mut self,
        _: &SelectNextDestination,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_destination(true, cx);
    }

    fn cycle_destination(&mut self, forward: bool, cx: &mut Context<Self>) {
        let next = logic::cycle_destination(self.destination, forward);
        self.set_destination(next, cx);
    }

    fn render_destination_row(
        &self,
        label: &'static str,
        description: &'static str,
        destination: Destination,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let colors = cx.theme().colors;
        let selected = self.destination == destination;
        let connection_name = self
            .open
            .as_ref()
            .map(|open| open.connection_name.clone())
            .unwrap_or_default();

        let mut title = div().flex().flex_row().items_baseline().gap_1().child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.text_primary))
                .child(label),
        );
        if destination == Destination::Connection && !connection_name.is_empty() {
            title = title.child(
                div()
                    .font_family(&cx.theme().fonts.data)
                    .text_size(px(11.5))
                    .text_color(rgb(colors.text_secondary))
                    .child(format!("\u{b7} {connection_name}")),
            );
        }

        div()
            .id(SharedString::from(format!(
                "save-modal-destination-{}",
                destination_slug(destination)
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(11.0))
            .px(px(12.0))
            .py(px(9.0))
            .rounded(px(7.0))
            .border_1()
            .cursor_pointer()
            .map(|el| {
                if selected {
                    el.border_color(rgba((colors.accent << 8) | 0x73))
                        .bg(rgba((colors.accent << 8) | 0x12))
                } else {
                    el.border_color(rgb(colors.border))
                        .hover(|el| el.bg(rgb(colors.bg_raised)))
                }
            })
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.set_destination(destination, cx);
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .size(px(13.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if selected {
                        colors.accent
                    } else {
                        colors.text_tertiary
                    }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(selected, |el| {
                        el.child(div().size(px(5.0)).rounded_full().bg(rgb(colors.accent)))
                    }),
            )
            .child(
                div().flex_1().flex().flex_col().child(title).child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(colors.text_secondary))
                        .child(description),
                ),
            )
    }

    /// The name field with the fixed `.sql` suffix rendered inside its box:
    /// the field itself draws no border, and this wrapper paints the border
    /// (accent while the field is focused) around field and suffix together.
    fn render_name_row(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let focused = self.name_field.read(cx).focus_handle(cx).is_focused(window);
        div()
            .flex()
            .flex_row()
            .items_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(if focused {
                colors.accent
            } else {
                colors.border
            }))
            .bg(rgb(colors.bg_app))
            .pr(px(10.0))
            .child(div().flex_1().child(self.name_field.clone()))
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(&cx.theme().fonts.data)
                    .text_size(px(theme::CONNECTION_FORM_RESULT_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(SQL_SUFFIX),
            )
    }

    /// The footer: the Up/Down destination hint (Save/Save-as only), then
    /// Cancel and the confirm button, which dims while the name does not
    /// validate.
    fn render_footer(
        &self,
        kind: SaveModalKind,
        can_save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let colors = cx.theme().colors;
        let mut footer = zsql_ui::modal::footer_bar(cx);
        if destination_shortcuts_active(kind) {
            footer = footer.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.text_tertiary))
                    .child("\u{2191}\u{2193} destination"),
            );
        }
        footer
            .child(div().flex_1())
            .child(
                secondary_button("save-modal-cancel", window, cx)
                    .track_focus(&self.cancel_focus)
                    .child("Cancel")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.close(cx);
                    })),
            )
            .child({
                let button = primary_button("save-modal-save", window, cx)
                    .track_focus(&self.save_focus)
                    .child(match kind {
                        SaveModalKind::Save | SaveModalKind::SaveAs => "Save script",
                        SaveModalKind::Rename => "Rename",
                    });
                if can_save {
                    button.on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.confirm(cx);
                    }))
                } else {
                    button.opacity(theme::CONNECTION_FORM_DIM_OPACITY)
                }
            })
    }

    fn render_error(&self, cx: &Context<Self>) -> Option<Div> {
        let colors = cx.theme().colors;
        let error = self.open.as_ref()?.error?;
        Some(
            div()
                .text_size(px(theme::CONNECTION_FORM_RESULT_TEXT_SIZE))
                .text_color(rgb(colors.status_error))
                .child(error.message()),
        )
    }
}

impl Render for SaveModalView {
    /// The caller is responsible for conditionally mounting this entity
    /// (only while [`Self::is_open`]), so `render` does not re-check that.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.refocus_name_field) {
            self.name_field.read(cx).focus_handle(cx).focus(window);
        }
        let kind = self.open.as_ref().map_or(SaveModalKind::Save, |o| o.kind);
        let title = match kind {
            SaveModalKind::Save | SaveModalKind::SaveAs => "Save script",
            SaveModalKind::Rename => "Rename",
        };
        let can_save = self.can_save();

        let mut body = div()
            .id("save-modal-body")
            .flex()
            .flex_col()
            .gap(px(15.0))
            .p_4();
        if destination_shortcuts_active(kind) {
            body = body
                .key_context(KEY_CONTEXT)
                .on_action(cx.listener(Self::select_previous_destination))
                .on_action(cx.listener(Self::select_next_destination));
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(7.0))
                .child(zsql_ui::modal::section_label("Name", None, cx))
                .child(self.render_name_row(window, cx)),
        );

        if destination_shortcuts_active(kind) {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(zsql_ui::modal::section_label("Where it lives", None, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(self.render_destination_row(
                                "This connection",
                                "Stays with this connection's tabs and scripts",
                                Destination::Connection,
                                cx,
                            ))
                            .child(self.render_destination_row(
                                "Library",
                                "Available on every connection",
                                Destination::Library,
                                cx,
                            ))
                            .child(self.render_destination_row(
                                "Somewhere else...",
                                "Export a copy to any folder - this tab stays put",
                                Destination::External,
                                cx,
                            )),
                    ),
            );
        }

        if let Some(error) = self.render_error(cx) {
            body = body.child(error);
        }

        let footer = self.render_footer(kind, can_save, window, cx);

        Modal::<Div, Div>::new("save-modal")
            .size(ModalSize::Small)
            .track_focus(&self.modal_focus)
            .on_close(cx.listener(|view, (), _w, cx| view.close(cx)))
            .has_close_icon(false)
            .head(zsql_ui::modal::head_with_esc_hint(title, cx))
            .body(div().flex().flex_col().child(body).child(footer))
    }
}

#[cfg(test)]
mod tests;
