//! The Open Script picker: a live filter field over two sections ("This
//! connection" and "Library") plus a "Browse files..." footer row, built
//! from `zsql_ui::Modal` (`ModalSize::Wide`) and `zsql_ui`'s text field --
//! the same construction idiom as the Save Script modal (see
//! `crate::ui::save_modal`).

mod bindings;
mod logic;

pub(crate) use bindings::OpenModalBindings;
use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, Render, Window,
    actions, div, prelude::*, px, rgb, rgba,
};
pub use logic::{
    LibraryScript, PickerRow, PickerRowMeta, PickerSection, PickerTarget, SessionScript,
    build_rows_with_open_sessions,
};
use zsql_ui::button::{primary_button, secondary_button};
use zsql_ui::modal::{Modal, ModalSize};
use zsql_ui::scrollable::{ScrollbarStyle, WithScrollbars};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::tabs::TabId;
use super::theme;

/// The key context [`OpenModalView`]'s arrow-key navigation is scoped to. An
/// ancestor of the filter field's own `TextField` key context in the render
/// tree, the same pattern `save_modal::KEY_CONTEXT` uses for its `1`/`2`
/// destination shortcuts: up/down resolve here since the field itself binds
/// neither, while every key the field does bind keeps working normally.
pub const KEY_CONTEXT: &str = "OpenModal";

actions!(open_modal, [SelectPreviousRow, SelectNextRow]);

/// Register this modal's arrow-key navigation bindings from `bindings`.
/// Call once at startup, before any window that hosts an [`OpenModalView`]
/// is opened.
pub fn init(cx: &mut App, bindings: &OpenModalBindings) {
    let mut keys = Vec::new();
    crate::keybindings::bind_all(
        &mut keys,
        &bindings.select_previous_row,
        &SelectPreviousRow,
        KEY_CONTEXT,
    );
    crate::keybindings::bind_all(
        &mut keys,
        &bindings.select_next_row,
        &SelectNextRow,
        KEY_CONTEXT,
    );
    let registered = keys.len();
    cx.bind_keys(keys);
    tracing::debug!(registered, "open modal keybindings registered");
}

/// What [`OpenModalView`] asks its parent (`ui::workspace::WorkspaceView`)
/// to do.
#[derive(Debug, Clone)]
pub enum OpenModalEvent {
    /// The user opened (or focused) `target`.
    Open(PickerTarget),
    /// The user chose "Browse files...".
    BrowseFiles,
    /// The user cancelled (Escape or the close icon). Nothing was opened.
    Cancelled,
}

impl EventEmitter<OpenModalEvent> for OpenModalView {}

/// The context a picker session was seeded with: everything [`logic::build_rows`]
/// needs, re-filtered on every filter-field keystroke.
struct OpenState {
    connection_name: String,
    sessions: Vec<SessionScript>,
    open_session_tabs: Vec<(String, TabId)>,
    library: Vec<LibraryScript>,
    open_library_tabs: Vec<(String, TabId)>,
}

/// The Open Script picker's state: whether it is open (and what it was
/// seeded with), the filter field, the currently visible/filtered rows, and
/// which one is selected.
pub struct OpenModalView {
    open: Option<OpenState>,
    filter_field: Entity<TextFieldState>,
    rows: Vec<PickerRow>,
    selected: Option<usize>,
    /// The filter text [`Self::rebuild_rows`] last built [`Self::rows`]
    /// from, so the field observer can tell a real edit apart from a
    /// caret-blink notify.
    last_filter: String,
    modal_focus: FocusHandle,
    /// Set by [`Self::open`], consumed by the next `render`: focuses the
    /// filter field so keystrokes reach it immediately.
    refocus_filter_field: bool,
    /// Scroll state behind the row list: `rows_handle` tracks the
    /// scrollable region's own children (each header and row is a direct
    /// child, so [`gpui::ScrollHandle::scroll_to_item`] addresses them
    /// exactly), and `rows_scrollbar` is its overlaid scrollbar's chrome
    /// state. Together these let the row list scroll (rather than clip) once
    /// it overflows the modal, with keyboard/arrow selection scrolling the
    /// selected row into view.
    rows_handle: gpui::ScrollHandle,
    rows_scrollbar: Entity<zsql_ui::scrollable::ScrollableState>,
    /// Set whenever [`Self::selected`] changes, consumed by the next
    /// `render`: scrolls the newly selected row into view. Deferred to
    /// render (rather than computed immediately) since the flat child index
    /// depends on how many rows precede it in each section, which is only
    /// convenient to recompute alongside building the same rows for display.
    scroll_selected_into_view: bool,
}

impl OpenModalView {
    #[must_use]
    pub fn new(cx: &mut Context<Self>) -> Self {
        let filter_field = cx.new(|cx| TextFieldState::new("Filter scripts...", None, cx));
        cx.subscribe(&filter_field, |view, _field, event, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                view.confirm(cx);
            }
        })
        .detach();
        // Rebuild only when the filter text actually changed: the field
        // notifies for every visual change too (its caret blink loop ticks
        // roughly every half second while focused), and an unconditional
        // rebuild would reset the arrow-key selection back to the first row
        // on every tick.
        cx.observe(&filter_field, |view, field, cx| {
            if field.read(cx).value().as_ref() != view.last_filter {
                view.rebuild_rows(cx);
            }
        })
        .detach();

        Self {
            open: None,
            filter_field,
            rows: Vec::new(),
            selected: None,
            last_filter: String::new(),
            modal_focus: cx.focus_handle(),
            refocus_filter_field: false,
            rows_handle: gpui::ScrollHandle::new(),
            rows_scrollbar: cx.new(zsql_ui::scrollable::ScrollableState::new),
            scroll_selected_into_view: false,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Rows visible under the current filter text, for a caller (e.g. a
    /// test) that wants to assert on them directly.
    #[cfg(test)]
    #[must_use]
    pub fn rows_for_test(&self) -> &[PickerRow] {
        &self.rows
    }

    #[cfg(test)]
    #[must_use]
    pub fn selected_for_test(&self) -> Option<usize> {
        self.selected
    }

    /// Open the picker, seeded with `connection_name` (the active
    /// connection's display name) and every row
    /// [`logic::build_rows_with_open_sessions`] needs: the connection's
    /// named session scripts (a disk scan, not open tabs) and which of them
    /// are already open, and the same pair for the library.
    // Every parameter is an independent, already-resolved piece of state
    // the modal needs to seed and filter against; grouping them into a
    // wrapper struct would only move the field list, not shrink it (see
    // `WorkspaceView::new`'s identical justification).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        connection_name: String,
        sessions: Vec<SessionScript>,
        open_session_tabs: Vec<(String, TabId)>,
        library: Vec<LibraryScript>,
        open_library_tabs: Vec<(String, TabId)>,
        cx: &mut Context<Self>,
    ) {
        self.filter_field
            .update(cx, |field, cx| field.set_value("", cx));
        self.open = Some(OpenState {
            connection_name,
            sessions,
            open_session_tabs,
            library,
            open_library_tabs,
        });
        self.refocus_filter_field = true;
        self.rebuild_rows(cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            self.rows.clear();
            self.selected = None;
            cx.emit(OpenModalEvent::Cancelled);
            cx.notify();
        }
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        let filter = self.filter_field.read(cx).value().to_string();
        self.last_filter.clone_from(&filter);
        self.rows = logic::build_rows_with_open_sessions(
            &filter,
            &open.sessions,
            &open.open_session_tabs,
            &open.library,
            &open.open_library_tabs,
        );
        self.selected = if self.rows.is_empty() { None } else { Some(0) };
        self.scroll_selected_into_view = true;
        cx.notify();
    }

    fn select_previous(
        &mut self,
        _: &SelectPreviousRow,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = logic::navigate(self.rows.len(), self.selected, false);
        self.scroll_selected_into_view = true;
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNextRow, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected = logic::navigate(self.rows.len(), self.selected, true);
        self.scroll_selected_into_view = true;
        cx.notify();
    }

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        self.confirm(cx);
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let target = row.target.clone();
        self.open = None;
        self.rows.clear();
        self.selected = None;
        cx.emit(OpenModalEvent::Open(target));
        cx.notify();
    }

    fn browse_files(&mut self, cx: &mut Context<Self>) {
        self.open = None;
        self.rows.clear();
        self.selected = None;
        cx.emit(OpenModalEvent::BrowseFiles);
        cx.notify();
    }

    /// The row list's scrollbar chrome, matching the connection modal's own
    /// list scrollbar constants.
    fn rows_scrollbar_style(active_theme: &Theme) -> ScrollbarStyle {
        ScrollbarStyle::themed(
            &active_theme.colors,
            f32::from(theme::MODAL_LIST_SCROLLBAR_WIDTH),
            theme::MODAL_LIST_SCROLLBAR_RADIUS,
            f32::from(theme::MODAL_LIST_SCROLLBAR_GAP),
        )
    }

    /// A section header row: the shared modal section label with this
    /// picker's own list padding.
    fn render_section_header(
        label: &'static str,
        connection: Option<String>,
        cx: &Context<Self>,
    ) -> Div {
        let suffix = connection.map(|name| format!("\u{b7} {name}"));
        div()
            .flex_shrink_0()
            .px_1()
            .pt_1()
            .child(zsql_ui::modal::section_label(label, suffix, cx))
    }

    /// The "Browse files..." row under the sections, with its keyboard
    /// shortcut on the right.
    fn render_browse_files_row(cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let colors = cx.theme().colors;
        div()
            .id("open-modal-browse-files")
            .flex()
            .flex_row()
            .items_center()
            .h(px(30.0))
            .px(px(9.0))
            .pt(px(8.0))
            .border_t_1()
            .border_color(rgb(colors.border_soft))
            .cursor_pointer()
            .text_color(rgb(colors.text_secondary))
            .hover(|el| el.text_color(rgb(colors.text_primary)))
            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                view.browse_files(cx);
            }))
            .child(div().flex_1().child("Browse files..."))
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(&cx.theme().fonts.data)
                    .text_size(px(10.0))
                    .text_color(rgb(colors.text_tertiary))
                    .child("Ctrl+Shift+O"),
            )
    }

    /// The footer: the navigate/open hint on the left and the Open button,
    /// which dims while no row is selected.
    fn render_footer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let can_open = self.selected.is_some();
        zsql_ui::modal::footer_bar(cx)
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.text_tertiary))
                    .child("\u{2191}\u{2193} navigate \u{b7} \u{23ce} open"),
            )
            .child(div().flex_1())
            .child(
                secondary_button("open-modal-cancel", window, cx)
                    .child("Cancel")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.close(cx);
                    })),
            )
            .child({
                let button = primary_button("open-modal-open", window, cx).child("Open");
                if can_open {
                    button.on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.confirm(cx);
                    }))
                } else {
                    button.opacity(theme::CONNECTION_FORM_DIM_OPACITY)
                }
            })
    }

    fn render_row(
        &self,
        index: usize,
        row: &PickerRow,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let colors = cx.theme().colors;
        let selected = self.selected == Some(index);
        let meta = match &row.meta {
            PickerRowMeta::Open => "open".to_owned(),
            PickerRowMeta::RelativeTime(text) => text.clone(),
        };
        div()
            .id(("open-modal-row", index))
            .flex()
            .flex_shrink_0()
            .flex_row()
            .items_center()
            .gap(px(9.0))
            .h(px(28.0))
            .px(px(9.0))
            .rounded(px(theme::MODAL_ROW_RADIUS))
            .cursor_pointer()
            .font_family(&cx.theme().fonts.data)
            .map(|el| {
                if selected {
                    el.bg(rgba((colors.accent << 8) | 0x1a))
                } else {
                    el.hover(|el| el.bg(rgb(colors.bg_raised)))
                }
            })
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.select_row(index, cx);
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(13.0))
                    .text_size(px(10.0))
                    .text_color(rgb(colors.text_secondary))
                    .child("\u{2263}"),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.5))
                    .text_color(rgb(colors.text_primary))
                    .child(row.label.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(10.0))
                    .text_color(rgb(if matches!(row.meta, PickerRowMeta::Open) {
                        colors.accent
                    } else {
                        colors.text_tertiary
                    }))
                    .child(meta),
            )
    }
}

impl Render for OpenModalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.refocus_filter_field) {
            self.filter_field.read(cx).focus_handle(cx).focus(window);
        }
        let connection_name = self
            .open
            .as_ref()
            .map(|o| o.connection_name.clone())
            .unwrap_or_default();

        let connection_count = self
            .rows
            .iter()
            .filter(|row| row.section == PickerSection::Connection)
            .count();

        // One flat list of direct children -- both section headers and
        // every row -- so `self.rows_handle` (tracked on the scrollable
        // container these become children of) can address any one of them
        // by a plain child index for scroll-into-view.
        let mut items: Vec<gpui::AnyElement> = vec![
            Self::render_section_header("This connection", Some(connection_name), cx)
                .into_any_element(),
        ];
        for (index, row) in self.rows.iter().enumerate() {
            if row.section == PickerSection::Connection {
                items.push(self.render_row(index, row, cx).into_any_element());
            }
        }
        items.push(Self::render_section_header("Library", None, cx).into_any_element());
        for (index, row) in self.rows.iter().enumerate() {
            if row.section == PickerSection::Library {
                items.push(self.render_row(index, row, cx).into_any_element());
            }
        }

        if std::mem::take(&mut self.scroll_selected_into_view)
            && let Some(selected) = self.selected
        {
            let flat_index = logic::flat_row_index(selected, connection_count);
            self.rows_handle.scroll_to_item(flat_index);
        }

        self.rows_scrollbar.update(cx, |scroll, _cx| {
            scroll.vertical(zsql_ui::scrollable::Axis::measured(
                zsql_ui::scrollable::ScrollSource::Container(self.rows_handle.clone()),
            ));
        });

        let rows_list = div()
            .id("open-modal-rows")
            .flex()
            .flex_col()
            .gap_1()
            .flex_shrink_0()
            .max_h(theme::OPEN_MODAL_ROWS_MAX_HEIGHT)
            .overflow_y_scroll()
            .track_scroll(&self.rows_handle)
            .children(items)
            .with_scrollbars(
                &self.rows_scrollbar,
                Self::rows_scrollbar_style(cx.theme()),
                cx,
            );

        let body = div()
            .id("open-modal-body")
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p_4()
            .child(self.filter_field.clone())
            .child(rows_list)
            .child(Self::render_browse_files_row(cx));

        let footer = self.render_footer(window, cx);

        Modal::<Div, Div>::new("open-modal")
            .size(ModalSize::Wide)
            .track_focus(&self.modal_focus)
            .on_close(cx.listener(|view, (), _w, cx| view.close(cx)))
            .has_close_icon(false)
            .head(zsql_ui::modal::head_with_esc_hint("Open script", cx))
            .body(div().flex().flex_col().child(body).child(footer))
    }
}

#[cfg(test)]
mod tests;
