//! [`ConnectionManagerView`]'s rendering: the modal shell, title bar,
//! saved-connections list (rows, icon buttons, status line), and the
//! add/edit form panel (which just hands off to [`super::form::ConnectionForm`]'s
//! own `Render` impl).

use gpui::{ClickEvent, Context, Div, KeyDownEvent, Render, Window, div, prelude::*, px, rgb};
use zsql_ui::modal::Modal;
use zsql_ui::theme::ActiveTheme;

use super::{ConnectionManagerView, ManagerView};
use crate::ui::connections::list::{ConnectionList, ConnectionListEvent};
use crate::ui::theme;

impl Render for ConnectionManagerView {
    /// The modal overlay: a dimmed backdrop (clicking it closes the modal)
    /// centering a panel that shows either the list or the add/edit form.
    /// Only ever mounted while [`Self::is_open`] is true -- the caller
    /// (`ui::workspace::WorkspaceView`) is responsible for conditionally
    /// mounting this entity in the first place, so `render` does not
    /// re-check `open` itself.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div().on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
            view.handle_modal_key_down(event, window, cx);
        }));
        body = body.child(match self.current_view() {
            ManagerView::List => self.render_modal_list(window, cx).into_any_element(),
            ManagerView::Form => self.render_modal_form(window, cx).into_any_element(),
        });
        Modal::<Div, Div>::new("connection-modal")
            .track_focus(&self.modal_focus)
            .on_close(cx.listener(|view, _, _w, cx| view.close(cx)))
            .head(self.render_modal_head(cx))
            .body(body)
    }
}

impl ConnectionManagerView {
    /// The modal's title bar: a back arrow on the form, the panel title
    /// (naming the connection being edited, for the edit form), a
    /// saved-count subtitle on the list, and a close (`x`) button.
    fn render_modal_head(&self, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let mut head = div().flex().flex_row().items_center();

        if !matches!(self.current_view(), ManagerView::List) {
            head = head.child(
                div()
                    .id("connection-form-back")
                    .cursor_pointer()
                    .pr_2()
                    .text_color(rgb(colors.text_tertiary))
                    .child("<")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.cancel_form(cx);
                    })),
            );
        }

        let is_edit = self.form.read(cx).is_edit();
        let title = match self.current_view() {
            ManagerView::List => "Connections".to_owned(),
            ManagerView::Form if is_edit => "Edit connection".to_owned(),
            ManagerView::Form => "Add connection".to_owned(),
        };
        head = head.child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(colors.text_primary))
                .child(title),
        );

        if matches!(self.current_view(), ManagerView::List) {
            head = head.child(
                div()
                    .pl_2()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(format!("{} saved", self.rows.len())),
            );
        }
        if matches!(self.current_view(), ManagerView::Form) && is_edit {
            let name = self.form.read(cx).input_values(cx).0;
            head = head.child(
                div()
                    .pl_2()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(name),
            );
        }

        head
    }

    /// The saved-connections list panel: every row plus the "Add
    /// connection" affordance and the inline status line.
    fn render_modal_list(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ConnectionList::with_connections(
            self.connections()
                .iter()
                .enumerate()
                .map(|(i, c)| (c, self.row_focus_handles.get(i).cloned())),
        )
        .status(self.status.clone())
        .active_id(self.active().and_then(|a| a.id))
        .on_event(cx.listener(|view, event, window, cx| match event {
            ConnectionListEvent::Add => view.show_add_form(window, cx),
            ConnectionListEvent::Edit { id } => view.show_edit_form(*id, window, cx),
            ConnectionListEvent::Connect { id } => view.connect_and_close(*id, cx).detach(),
            ConnectionListEvent::Delete { id } => {
                if let Err(e) = view.delete_id(*id, cx) {
                    tracing::error!("failed to delete connection: {e}");
                }
            }
        }))
    }

    /// The add/edit form panel: [`super::form::ConnectionForm`] draws its own
    /// fields, driver-conditional section, test-outcome banner, and footer.
    fn render_modal_form(&self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.form.clone()
    }
}
