use std::rc::Rc;

use gpui::{
    App, ClickEvent, Div, FocusHandle, RenderOnce, Stateful, Window, div, prelude::*, px, rgb, rgba,
};
use uuid::Uuid;
use zsql_ui::{
    button::secondary_button,
    grid,
    icon::{IconName, icon},
    theme::ActiveTheme,
};

use crate::ui::{connections::ConnectionRow, theme};

/// The callback [`ConnectionList`]/[`ConnectionListItem`] report user intent
/// through, shared by both since every row's listener is cloned from the
/// list's own.
type EventListener = Rc<dyn Fn(&ConnectionListEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement, Clone)]
pub struct ConnectionList {
    connection_rows: Vec<ConnectionListItem>,
    status_text: Option<String>,
    event_listener: EventListener,
}

pub enum ConnectionListEvent {
    Add,
    Delete { id: Uuid },
    Edit { id: Uuid },
    Connect { id: Uuid },
}

impl ConnectionList {
    pub fn with_connections<'a>(
        connections: impl IntoIterator<Item = (&'a ConnectionRow, Option<FocusHandle>)>,
    ) -> Self {
        Self {
            connection_rows: connections
                .into_iter()
                .enumerate()
                .map(|(i, c)| ConnectionListItem::from_row(c.0, i, c.1))
                .collect(),
            status_text: None,
            event_listener: Rc::new(|_e, _w, _cx| {}),
        }
    }

    pub fn status(mut self, status: Option<String>) -> Self {
        self.status_text = status;
        self
    }

    pub fn active_id(mut self, active: Option<Uuid>) -> Self {
        for row in &mut self.connection_rows {
            row.is_active = Some(row.id) == active;
        }
        self
    }

    pub fn on_event(
        mut self,
        listener: impl Fn(&ConnectionListEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.event_listener = Rc::new(listener);
        for row in &mut self.connection_rows {
            row.event_listener = self.event_listener.clone();
        }
        self
    }
}

impl ConnectionList {
    fn render_status(cx: &App, status: Option<String>) -> Div {
        let text = status.unwrap_or("click a row to connect\t•\tesc to close".to_string());
        div()
            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
            .text_color(rgb(cx.theme().colors.text_tertiary))
            .child(text)
    }
}

impl RenderOnce for ConnectionList {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        let colors = cx.theme().colors;
        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .max_h(theme::MODAL_LIST_MAX_HEIGHT)
            .overflow_hidden();
        for child in self.connection_rows.drain(..) {
            list = list.child(child);
        }
        let event_listener = self.event_listener;
        div().flex().flex_col().child(list).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(rgb(colors.border_soft))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .child(
                            secondary_button("add-connection-button", window, cx)
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                // Lets render tests find this button's painted
                                // bounds via `VisualTestContext::debug_bounds`
                                // -- a no-op outside test/test-support builds.
                                .debug_selector(|| "connection-list-add-button".to_owned())
                                .child(icon(
                                    IconName::Add,
                                    theme::MODAL_ADD_ICON_SIZE,
                                    colors.accent,
                                ))
                                .child("Add connection")
                                .on_click(move |_c, w, cx| {
                                    event_listener(&ConnectionListEvent::Add, w, cx);
                                }),
                        )
                        .child(Self::render_status(cx, self.status_text)),
                ),
        )
    }
}

#[derive(IntoElement, Clone)]
struct ConnectionListItem {
    id: Uuid,
    index: usize,
    name: String,
    host: String,
    driver: String,
    is_active: bool,
    event_listener: EventListener,
    focus_handle: Option<FocusHandle>,
}

impl ConnectionListItem {
    pub fn from_row(
        value: &ConnectionRow,
        index: usize,
        focus_handle: Option<FocusHandle>,
    ) -> Self {
        Self {
            id: value.connection.id,
            index,
            name: value.connection.name.clone(),
            host: value.connection.display_host.clone(),
            driver: value.connection.display_kind.clone(),
            is_active: false,
            event_listener: Rc::new(|_e, _w, _a| {}),
            focus_handle,
        }
    }

    /// A row's name (+ "connected" label when active) and url, stacked.
    fn render_row_meta(&self, colors: zsql_ui::theme::Colors) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(theme::MODAL_ROW_INNER_GAP)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .text_size(px(theme::MODAL_ROW_NAME_TEXT_SIZE))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(colors.text_primary))
                            .child(self.name.clone()),
                    )
                    .when(self.is_active, |el| {
                        el.child(
                            div()
                                .text_size(px(theme::MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE))
                                .text_color(rgb(colors.accent))
                                .child("connected"),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(theme::MODAL_ROW_URL_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .truncate()
                    .child(self.host.clone()),
            )
    }
}

impl RenderOnce for ConnectionListItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let row_meta = self.render_row_meta(colors);
        let focus_handle = self.focus_handle.unwrap_or_else(|| cx.focus_handle());
        let connect_listener = self.event_listener.clone();
        let enter_connect_listener = self.event_listener.clone();
        let edit_listener = self.event_listener.clone();
        let delete_listener = self.event_listener;

        let mut item = div()
            .id(("connection-modal-row", self.index))
            .track_focus(&focus_handle)
            // Lets render tests find this row's painted bounds via
            // `VisualTestContext::debug_bounds` -- a no-op outside
            // test/test-support builds.
            .debug_selector(move || format!("connection-list-row-{}", self.index))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(theme::MODAL_ROW_RADIUS))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .on_click(move |_e, w, cx| {
                connect_listener(&ConnectionListEvent::Connect { id: self.id }, w, cx);
            })
            .on_key_down(move |e, w, cx| {
                if e.keystroke.key == "enter" {
                    enter_connect_listener(&ConnectionListEvent::Connect { id: self.id }, w, cx);
                }
            })
            .child(if self.is_active {
                grid::status_dot(colors.accent)
            } else {
                grid::status_dot_outline(colors.text_tertiary)
            })
            .child(row_meta)
            .child(grid::type_tag(&self.driver, active_theme))
            .child(
                RowIconButton {
                    id_name: "edit-connection-button",
                    index: self.index,
                    icon_name: IconName::Edit,
                    icon_size: theme::MODAL_EDIT_ICON_SIZE,
                    idle_color: colors.text_tertiary,
                    hover_color: colors.text_secondary,
                }
                .render(move |_e, w, cx| {
                    edit_listener(&ConnectionListEvent::Edit { id: self.id }, w, cx);
                }),
            )
            .child(
                RowIconButton {
                    id_name: "delete-connection-button",
                    index: self.index,
                    icon_name: IconName::Delete,
                    icon_size: theme::MODAL_DELETE_ICON_SIZE,
                    idle_color: colors.text_tertiary,
                    hover_color: colors.status_error,
                }
                .render(move |_e, w, cx| {
                    delete_listener(&ConnectionListEvent::Delete { id: self.id }, w, cx);
                }),
            );

        if self.is_active {
            item = item.bg(rgba(theme::modal_row_active_bg(active_theme)));
        }
        item
    }
}

/// The identity and coloring of one list row's trailing icon button (edit or
/// delete); see [`ConnectionManagerView::row_icon_button`].
#[derive(Clone, Copy)]
struct RowIconButton {
    id_name: &'static str,
    index: usize,
    icon_name: IconName,
    icon_size: gpui::Pixels,
    idle_color: u32,
    hover_color: u32,
}

impl RowIconButton {
    pub fn render(
        self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let hover_group = format!("{}-hover-{}", self.id_name, self.index);
        div()
            .id((self.id_name, self.index))
            .group(hover_group.clone())
            .cursor_pointer()
            .px_1()
            // Lets render tests find this button's painted bounds via
            // `VisualTestContext::debug_bounds` -- a no-op outside
            // test/test-support builds.
            .debug_selector(move || format!("connection-list-{}-{}", self.id_name, self.index))
            .on_click(move |e, w, cx| {
                cx.stop_propagation();
                on_click(e, w, cx);
            })
            .child(
                icon(self.icon_name, self.icon_size, self.idle_color)
                    .group_hover(hover_group, move |style| {
                        style.text_color(rgb(self.hover_color))
                    }),
            )
    }
}

/// [`ConnectionList`]/[`ConnectionListItem`] tested in isolation, with no
/// [`super::ConnectionManagerView`] involved.
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        Context, Entity, FocusHandle, Modifiers, Render, TestAppContext, VisualTestContext, Window,
        prelude::*,
    };
    use uuid::Uuid;

    use super::{ConnectionList, ConnectionListEvent};
    use crate::connections::ConnectionArgs;
    use crate::ui::connections::ConnectionRow;

    fn sample_row(name: &str, url: &str) -> ConnectionRow {
        ConnectionRow {
            connection: ConnectionArgs {
                name: name.to_owned(),
                url: url.to_owned(),
                ssh: None,
                ssh_secret: None,
            }
            .into_stored()
            .expect("into_stored must succeed for a well-formed url"),
        }
    }

    /// A plain-data mirror of [`ConnectionListEvent`] a test can capture,
    /// compare, and print without requiring the production event type
    /// itself to carry those trait implementations.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum CapturedEvent {
        Add,
        Delete(Uuid),
        Edit(Uuid),
        Connect(Uuid),
    }

    impl From<&ConnectionListEvent> for CapturedEvent {
        fn from(event: &ConnectionListEvent) -> Self {
            match event {
                ConnectionListEvent::Add => CapturedEvent::Add,
                ConnectionListEvent::Delete { id } => CapturedEvent::Delete(*id),
                ConnectionListEvent::Edit { id } => CapturedEvent::Edit(*id),
                ConnectionListEvent::Connect { id } => CapturedEvent::Connect(*id),
            }
        }
    }

    /// A minimal host entity that renders a built [`ConnectionList`] and
    /// forwards every event it reports into a shared, test-owned capture
    /// list -- exists only so the `RenderOnce` list component can be driven
    /// through a real window, with no [`super::super::ConnectionManagerView`]
    /// involved.
    struct ListHost {
        rows: Vec<ConnectionRow>,
        row_focus_handles: Vec<FocusHandle>,
        active_id: Option<Uuid>,
        events: Rc<RefCell<Vec<CapturedEvent>>>,
    }

    impl ListHost {
        fn new(
            rows: Vec<ConnectionRow>,
            events: Rc<RefCell<Vec<CapturedEvent>>>,
            cx: &mut Context<Self>,
        ) -> Self {
            let row_focus_handles = rows.iter().map(|_| cx.focus_handle()).collect();
            Self {
                rows,
                row_focus_handles,
                active_id: None,
                events,
            }
        }
    }

    impl Render for ListHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let events = self.events.clone();
            ConnectionList::with_connections(
                self.rows
                    .iter()
                    .zip(self.row_focus_handles.iter().cloned().map(Some)),
            )
            .active_id(self.active_id)
            .on_event(move |event, _window, _cx| {
                events.borrow_mut().push(CapturedEvent::from(event));
            })
        }
    }

    fn build_host(
        cx: &mut TestAppContext,
        rows: Vec<ConnectionRow>,
    ) -> (
        Entity<ListHost>,
        &mut VisualTestContext,
        Rc<RefCell<Vec<CapturedEvent>>>,
    ) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let events_for_host = events.clone();
        let (host, vcx) =
            cx.add_window_view(|_window, cx| ListHost::new(rows, events_for_host, cx));
        (host, vcx, events)
    }

    #[gpui::test]
    fn rows_render_one_per_connection_with_the_expected_name_host_and_driver_tag(
        cx: &mut TestAppContext,
    ) {
        let rows = vec![
            sample_row("local pg", "postgres://localhost/app"),
            sample_row("local sqlite", "sqlite::memory:"),
        ];
        assert_eq!(rows[0].connection.display_kind, "PostgreSQL");
        assert_eq!(rows[0].connection.display_host, "localhost");
        assert_eq!(rows[1].connection.display_kind, "SQLite");

        let (_host, vcx, _events) = build_host(cx, rows);
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds("connection-list-row-0").is_some(),
            "row 0 must actually paint, not just exist in the data model"
        );
        assert!(
            vcx.debug_bounds("connection-list-row-1").is_some(),
            "row 1 must actually paint, not just exist in the data model"
        );
    }

    #[gpui::test]
    fn an_unrecognized_scheme_surfaces_as_an_error_tag_not_a_panic(cx: &mut TestAppContext) {
        let rows = vec![sample_row("mystery", "cassandra://host/db")];
        assert_eq!(rows[0].connection.display_kind, "Unknown");

        let (_host, vcx, _events) = build_host(cx, rows);
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds("connection-list-row-0").is_some(),
            "the unrecognized-scheme row must still paint rather than panicking"
        );
    }

    #[gpui::test]
    fn clicking_a_row_emits_connect_with_the_clicked_row_id(cx: &mut TestAppContext) {
        let rows = vec![
            sample_row("a", "sqlite::memory:"),
            sample_row("b", "sqlite:///tmp/b.db"),
        ];
        let target_id = rows[1].connection.id;
        let (_host, vcx, events) = build_host(cx, rows);
        vcx.run_until_parked();

        let bounds = vcx
            .debug_bounds("connection-list-row-1")
            .expect("row 1 must be tagged and painted");
        vcx.simulate_click(bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Connect(target_id)]);
    }

    #[gpui::test]
    fn enter_on_a_focused_row_emits_connect(cx: &mut TestAppContext) {
        let rows = vec![sample_row("a", "sqlite::memory:")];
        let target_id = rows[0].connection.id;
        let (host, vcx, events) = build_host(cx, rows);
        vcx.run_until_parked();

        let row_handle = host.read_with(vcx, |host, _app| host.row_focus_handles[0].clone());
        vcx.update(|window, _cx| window.focus(&row_handle));
        vcx.run_until_parked();
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Connect(target_id)]);
    }

    #[gpui::test]
    fn clicking_the_delete_icon_emits_delete(cx: &mut TestAppContext) {
        let rows = vec![sample_row("a", "sqlite::memory:")];
        let target_id = rows[0].connection.id;
        let (_host, vcx, events) = build_host(cx, rows);
        vcx.run_until_parked();

        let bounds = vcx
            .debug_bounds("connection-list-delete-connection-button-0")
            .expect("the delete icon must be tagged and painted");
        vcx.simulate_click(bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Delete(target_id)]);
    }

    #[gpui::test]
    fn clicking_the_edit_icon_emits_edit(cx: &mut TestAppContext) {
        let rows = vec![sample_row("a", "sqlite::memory:")];
        let target_id = rows[0].connection.id;
        let (_host, vcx, events) = build_host(cx, rows);
        vcx.run_until_parked();

        let bounds = vcx
            .debug_bounds("connection-list-edit-connection-button-0")
            .expect("the edit icon must be tagged and painted");
        vcx.simulate_click(bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Edit(target_id)]);
    }

    #[gpui::test]
    fn clicking_the_add_connection_button_emits_add(cx: &mut TestAppContext) {
        let (_host, vcx, events) = build_host(cx, Vec::new());
        vcx.run_until_parked();

        let bounds = vcx
            .debug_bounds("connection-list-add-button")
            .expect("the add-connection button must be tagged and painted");
        vcx.simulate_click(bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Add]);
    }
}
