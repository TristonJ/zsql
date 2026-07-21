use gpui::{
    ClickEvent, Context, Div, Focusable, KeyDownEvent, Stateful, Window, div, prelude::*, px, rgb,
    rgba,
};
use zsql_ui::button::secondary_button;
use zsql_ui::grid;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::theme::ActiveTheme;

use super::super::theme;
use super::{ConnectionManagerView, ConnectionRow, ManagerView, form, style};

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

impl ConnectionManagerView {
    /// The saved-connections list panel: every row plus the "Add
    /// connection" affordance and the inline status line.
    pub(super) fn render_modal_head(&self, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(style::MODAL_HEAD_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(rgb(colors.border_soft));

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

        let title = match self.current_view() {
            ManagerView::List => "Connections".to_owned(),
            ManagerView::AddForm => "Add connection".to_owned(),
            ManagerView::EditForm { .. } => "Edit connection".to_owned(),
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
        if matches!(self.current_view(), ManagerView::EditForm { .. }) {
            let name = self.name_field.read(cx).value().to_string();
            head = head.child(
                div()
                    .pl_2()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(name),
            );
        }

        head.child(
            div()
                .id("connection-modal-close")
                .group(style::MODAL_CLOSE_HOVER_GROUP)
                .ml_auto()
                .cursor_pointer()
                .child(
                    icon(
                        IconName::Close,
                        style::MODAL_CLOSE_ICON_SIZE,
                        colors.text_tertiary,
                    )
                    .group_hover(style::MODAL_CLOSE_HOVER_GROUP, |style| {
                        style.text_color(rgb(colors.text_primary))
                    }),
                )
                .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                    view.close(cx);
                })),
        )
    }

    pub(super) fn render_modal_list(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .max_h(style::MODAL_LIST_MAX_HEIGHT)
            .overflow_hidden();
        for (index, row) in self.connections().iter().enumerate() {
            list = list.child(self.render_modal_row(index, row, cx));
        }

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
                                .child(icon(
                                    IconName::Add,
                                    style::MODAL_ADD_ICON_SIZE,
                                    colors.accent,
                                ))
                                .child("Add connection")
                                .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                                    view.show_add_form(cx);
                                    let handle = view.name_field.read(cx).focus_handle(cx);
                                    window.focus(&handle);
                                })),
                        )
                        .child(self.render_status(cx)),
                ),
        )
    }

    /// One connection-list row: status dot, name (+ "connected" label and
    /// teal tint when this row is the active connection), url, driver tag,
    /// an edit affordance, and a delete affordance. Clicking the row's body
    /// connects to it and closes the modal; `Enter` while the row is
    /// focused does the same. The edit/delete controls stop propagation so
    /// neither triggers the row's own connect. Deliberately has no
    /// `border_l` rail on the active row -- the teal dot, label, and
    /// background tint are the only "this one is active" cues.
    fn render_modal_row(
        &self,
        index: usize,
        row: &ConnectionRow,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let driver_label = match &row.driver_id {
            Ok(id) => form::driver_display_label(id).to_owned(),
            Err(_) => "unrecognized".to_owned(),
        };
        let is_active = self.active.as_ref().is_some_and(|active| {
            active.name == row.connection.name && active.url == row.connection.url
        });
        let focus_handle = self
            .row_focus_handles
            .get(index)
            .cloned()
            .unwrap_or_else(|| cx.focus_handle());

        let mut item = div()
            .id(("connection-modal-row", index))
            .track_focus(&focus_handle)
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(style::MODAL_ROW_RADIUS))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.connect_and_close(index, cx).detach();
            }))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "enter" {
                    view.connect_and_close(index, cx).detach();
                }
            }))
            .child(if is_active {
                grid::status_dot(colors.accent)
            } else {
                grid::status_dot_outline(colors.text_tertiary)
            })
            .child(Self::render_row_meta(row, is_active, colors))
            .child(grid::type_tag(&driver_label, active_theme))
            .child(Self::row_icon_button(
                cx,
                RowIconButton {
                    id_name: "edit-connection-button",
                    index,
                    icon_name: IconName::Edit,
                    icon_size: style::MODAL_EDIT_ICON_SIZE,
                    idle_color: colors.text_tertiary,
                    hover_color: colors.text_secondary,
                },
                move |view, cx| view.show_edit_form(index, cx),
            ))
            .child(Self::row_icon_button(
                cx,
                RowIconButton {
                    id_name: "delete-connection-button",
                    index,
                    icon_name: IconName::Delete,
                    icon_size: style::MODAL_DELETE_ICON_SIZE,
                    idle_color: colors.text_tertiary,
                    hover_color: colors.status_error,
                },
                move |view, cx| {
                    let _ = view.delete_index(index, cx);
                },
            ));

        if is_active {
            item = item.bg(rgba(style::modal_row_active_bg(active_theme)));
        }
        item
    }

    /// A row's name (+ "connected" label when active) and url, stacked.
    fn render_row_meta(
        row: &ConnectionRow,
        is_active: bool,
        colors: zsql_ui::theme::Colors,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(style::MODAL_ROW_INNER_GAP)
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
                            .text_size(px(style::MODAL_ROW_NAME_TEXT_SIZE))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(colors.text_primary))
                            .child(row.connection.name.clone()),
                    )
                    .when(is_active, |el| {
                        el.child(
                            div()
                                .text_size(px(style::MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE))
                                .text_color(rgb(colors.accent))
                                .child("connected"),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(style::MODAL_ROW_URL_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .truncate()
                    .child(row.connection.url.clone()),
            )
    }

    /// One of a list row's trailing icon buttons (edit/delete): a
    /// hover-tinted icon whose click stops propagation -- so it never also
    /// dispatches the row's own connect-on-click -- before running
    /// `on_click`.
    fn row_icon_button(
        cx: &Context<Self>,
        spec: RowIconButton,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let hover_group = format!("{}-hover-{}", spec.id_name, spec.index);
        div()
            .id((spec.id_name, spec.index))
            .group(hover_group.clone())
            .cursor_pointer()
            .px_1()
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                on_click(view, cx);
            }))
            .child(
                icon(spec.icon_name, spec.icon_size, spec.idle_color)
                    .group_hover(hover_group, move |style| {
                        style.text_color(rgb(spec.hover_color))
                    }),
            )
    }
}
