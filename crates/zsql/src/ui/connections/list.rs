use std::rc::Rc;

use gpui::{
    App, ClickEvent, Context, Div, FocusHandle, Render, RenderOnce, Stateful, Window, div,
    prelude::*, px, rgb,
};
use uuid::Uuid;
use zsql_ui::{
    button::secondary_button,
    grid,
    icon::{IconName, icon},
    scrollable::{ScrollView, ScrollbarStyle, vertical_scroll},
    theme::{ActiveTheme, Theme},
};

use crate::ui::{connections::ConnectionRow, theme};

/// The callback [`ConnectionList`]/[`ConnectionListItem`] report user intent
/// through, shared by both since every row's listener is cloned from the
/// list's own.
type EventListener = Rc<dyn Fn(&ConnectionListEvent, &mut Window, &mut App) + 'static>;

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
        let text = status.unwrap_or("click a row to connect\t-\tesc to close".to_string());
        div()
            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
            .text_color(rgb(cx.theme().colors.text_tertiary))
            .child(text)
    }

    /// The connection list's scrollbar chrome: track/thumb thickness plus
    /// the active theme's scrollbar colors. The track paints no background.
    fn scrollbar_style(active_theme: &Theme) -> ScrollbarStyle {
        ScrollbarStyle::themed(
            &active_theme.colors,
            f32::from(theme::MODAL_LIST_SCROLLBAR_WIDTH),
            theme::MODAL_LIST_SCROLLBAR_RADIUS,
            f32::from(theme::MODAL_LIST_SCROLLBAR_GAP),
        )
    }

    /// Builds this list's element tree: the connection rows, scrollable
    /// within a viewport capped at `theme::MODAL_LIST_MAX_HEIGHT`, followed
    /// by the add-connection footer -- a sibling of the scrolled region, so
    /// it stays fully visible and clickable regardless of row count.
    ///
    /// `scroll_view` must be owned by the caller's own view (`V`) and
    /// persist across renders, the same as any other `ScrollView`: a fresh
    /// one every render would lose drag/measurement state and never settle.
    pub fn render<V: Render>(
        mut self,
        scroll_view: &ScrollView,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Div {
        let colors = cx.theme().colors;
        // `flex_shrink_0` keeps this row column at its natural (possibly
        // taller-than-viewport) height inside `vertical_scroll`'s flex
        // column, which would otherwise squeeze it down to fit rather than
        // letting it overflow and scroll.
        let mut rows = div().flex().flex_col().gap_1().p_1().flex_shrink_0();
        for child in self.connection_rows.drain(..) {
            rows = rows.child(child);
        }
        let scrollbar_style = Self::scrollbar_style(cx.theme());
        // Hugs the row column's own height up to the cap, so a short list
        // sits flush above the footer instead of leaving empty space; only
        // once the rows exceed the cap does this become the definite height
        // `vertical_scroll`'s own `h_full()` viewport resolves against.
        // `min_h_0` clears the flex min-content floor that would otherwise
        // let the row column's intrinsic height push this past its cap.
        let scrolled_rows = div()
            .flex()
            .flex_col()
            .min_h_0()
            .max_h(theme::MODAL_LIST_MAX_HEIGHT)
            .child(vertical_scroll(
                "connection-list-rows",
                scroll_view,
                scrollbar_style,
                rows,
                cx,
            ));

        let event_listener = self.event_listener;
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .child(scrolled_rows)
            .child(
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
                                    // Lets render tests find this button's
                                    // painted bounds via
                                    // `VisualTestContext::debug_bounds` -- a
                                    // no-op outside test/test-support builds.
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
                            .w_full()
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
            .child(grid::type_tag_accent(&self.driver, active_theme))
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
            item = item.bg(theme::modal_row_active_bg(active_theme));
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
        Context, Entity, FocusHandle, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, Render, TestAppContext, VisualTestContext, Window, point, prelude::*, px,
    };
    use uuid::Uuid;
    use zsql_ui::scrollable::{ScrollView, vertical_thumb_debug_selector};

    use super::{ConnectionList, ConnectionListEvent, theme};
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
    /// list -- exists only so the list component can be driven through a
    /// real window, with no [`super::super::ConnectionManagerView`]
    /// involved.
    struct ListHost {
        rows: Vec<ConnectionRow>,
        row_focus_handles: Vec<FocusHandle>,
        active_id: Option<Uuid>,
        events: Rc<RefCell<Vec<CapturedEvent>>>,
        list_scroll: ScrollView,
        /// How many times `render` has run, so a test can tell whether the
        /// list's first-frame scrollbar nudge settles after one extra
        /// render or keeps rescheduling itself.
        render_count: usize,
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
                list_scroll: ScrollView::new(cx),
                render_count: 0,
            }
        }
    }

    impl Render for ListHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.render_count += 1;
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
            .render(&self.list_scroll, window, cx)
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

    /// `count` distinct sqlite rows, named/ordered `conn-0`, `conn-1`, ...
    fn many_rows(count: usize) -> Vec<ConnectionRow> {
        (0..count)
            .map(|i| sample_row(&format!("conn-{i}"), &format!("sqlite:///tmp/conn-{i}.db")))
            .collect()
    }

    #[gpui::test]
    fn a_short_list_that_fits_the_capped_viewport_renders_with_no_scrollbar_thumb(
        cx: &mut TestAppContext,
    ) {
        let (host, vcx, _events) = build_host(cx, many_rows(4));
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds("connection-list-row-3").is_some(),
            "all 4 rows must fit inside the capped viewport and paint"
        );
        let scroll = host.read_with(vcx, |h, _app| h.list_scroll.scroll_state().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_none(),
            "a list that fits the capped viewport must render no scrollbar thumb"
        );
    }

    #[gpui::test]
    fn a_short_list_hugs_its_rows_instead_of_reserving_the_full_capped_height(
        cx: &mut TestAppContext,
    ) {
        let (_host, vcx, _events) = build_host(cx, many_rows(4));
        vcx.run_until_parked();

        let row3_bounds = vcx
            .debug_bounds("connection-list-row-3")
            .expect("row 3 must be painted");
        let footer_bounds = vcx
            .debug_bounds("connection-list-add-button")
            .expect("the add-connection footer must be painted");
        let half_cap = px(f32::from(theme::MODAL_LIST_MAX_HEIGHT) / 2.0);
        assert!(
            footer_bounds.origin.y < row3_bounds.origin.y + half_cap,
            "a short list must hug its rows rather than reserving the full capped height, \
             leaving a large empty gap above the footer"
        );
    }

    #[gpui::test]
    fn twelve_connections_stay_capped_with_the_footer_visible_and_a_scrollbar_thumb(
        cx: &mut TestAppContext,
    ) {
        let (host, vcx, _events) = build_host(cx, many_rows(12));
        vcx.run_until_parked();

        let row0_bounds = vcx
            .debug_bounds("connection-list-row-0")
            .expect("row 0 must be painted");
        let footer_bounds = vcx
            .debug_bounds("connection-list-add-button")
            .expect("the add-connection footer must stay painted with 12 connections");
        let half_cap = px(f32::from(theme::MODAL_LIST_MAX_HEIGHT) / 2.0);
        assert!(
            footer_bounds.origin.y >= row0_bounds.origin.y + half_cap,
            "the footer must sit below the capped scroll region, not just under row 0"
        );

        // Row 11 is still laid out (and so still has a recorded position)
        // even while scrolled out of view -- overflow clips what paints,
        // not what gets measured -- so "not reachable" means its position
        // falls at or beyond the footer, outside the visible rows region.
        let row11_bounds = vcx
            .debug_bounds("connection-list-row-11")
            .expect("row 11 is laid out even though it is clipped out of the capped viewport");
        assert!(
            row11_bounds.origin.y >= footer_bounds.origin.y,
            "row 11 must not be visible inside the capped viewport before scrolling"
        );

        let scroll = host.read_with(vcx, |h, _app| h.list_scroll.scroll_state().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_some(),
            "12 connections overflowing the capped viewport must show a vertical scrollbar thumb"
        );
    }

    #[gpui::test]
    fn the_add_connection_footer_stays_clickable_with_many_connections(cx: &mut TestAppContext) {
        let (_host, vcx, events) = build_host(cx, many_rows(12));
        vcx.run_until_parked();

        let bounds = vcx.debug_bounds("connection-list-add-button").expect(
            "the add-connection button must stay tagged and painted outside the \
                     scrolled rows region even with 12 connections",
        );
        vcx.simulate_click(bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Add]);
    }

    #[gpui::test]
    fn scrolling_the_capped_list_reveals_and_makes_clickable_a_row_hidden_beyond_the_viewport(
        cx: &mut TestAppContext,
    ) {
        let rows = many_rows(12);
        let target_id = rows[11].connection.id;
        let (host, vcx, events) = build_host(cx, rows);
        vcx.run_until_parked();

        let scroll = host.read_with(vcx, |h, _app| h.list_scroll.scroll_state().clone());
        let thumb_bounds = vcx
            .debug_bounds(vertical_thumb_debug_selector(&scroll))
            .expect("12 overflowing connections must show a scrollbar thumb to drag");

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_bounds.center(),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        let dragged_to = point(thumb_bounds.center().x, thumb_bounds.center().y + px(500.0));
        vcx.simulate_event(MouseMoveEvent {
            position: dragged_to,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: dragged_to,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        let row_bounds = vcx
            .debug_bounds("connection-list-row-11")
            .expect("row 11 must be reachable once the capped list is scrolled to its end");
        vcx.simulate_click(row_bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(*events.borrow(), vec![CapturedEvent::Connect(target_id)]);
    }

    #[gpui::test]
    fn idle_reparking_after_the_first_frame_does_not_keep_rescheduling_renders(
        cx: &mut TestAppContext,
    ) {
        let (host, vcx, _events) = build_host(cx, many_rows(12));
        vcx.run_until_parked();

        let scroll = host.read_with(vcx, |h, _app| h.list_scroll.scroll_state().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_some(),
            "the scrollbar must appear once the viewport is measured, with no further input"
        );
        let render_count_once_settled = host.read_with(vcx, |h, _app| h.render_count);

        vcx.run_until_parked();
        vcx.run_until_parked();
        let render_count_after_idle_parking = host.read_with(vcx, |h, _app| h.render_count);

        assert_eq!(
            render_count_after_idle_parking, render_count_once_settled,
            "once the viewport is measured, idle parking must not keep rescheduling renders"
        );
    }
}
