//! A relation row's right-click context menu: state, actions (copy name,
//! copy qualified name), and its anchored overlay.

use gpui::{ClipboardItem, Context, Pixels, Point, point, prelude::*};
use zsql_core::RelationKind;
use zsql_ui::context_menu::{ContextMenu, ContextMenuItem};
use zsql_ui::tree::ROW_HEIGHT;

use super::SidebarView;

/// A relation row's open right-click context menu: which relation it
/// targets, the flattened index of its triggering row (so the menu can
/// anchor to that row's right edge), and the triggering click position used
/// as a fallback anchor before the tree viewport has been measured.
#[derive(Debug, Clone)]
pub(super) struct ContextMenuState {
    schema: String,
    relation: String,
    kind: RelationKind,
    row_index: usize,
    fallback_position: Point<Pixels>,
}

/// Open the right-click context menu for `schema.relation`, anchored to the
/// right edge of its `row_index` row. `fallback_position` (window
/// coordinates, from the triggering mouse event) anchors the menu until the
/// tree viewport has been measured.
#[allow(clippy::too_many_arguments)]
pub(super) fn open(
    view: &mut SidebarView,
    schema: String,
    relation: String,
    kind: RelationKind,
    row_index: usize,
    fallback_position: Point<Pixels>,
    cx: &mut Context<SidebarView>,
) {
    view.context_menu = Some(ContextMenuState {
        schema,
        relation,
        kind,
        row_index,
        fallback_position,
    });
    cx.notify();
}

/// Where to anchor the context menu for the relation row at `row_index`:
/// the top of that row at the tree viewport's right edge, in window
/// coordinates. `None` before the tree viewport has been measured, when the
/// row's on-screen position cannot yet be derived.
#[allow(clippy::cast_precision_loss)]
pub(super) fn relation_row_anchor(view: &SidebarView, row_index: usize) -> Option<Point<Pixels>> {
    let bounds = view.tree_scroll_handle.0.borrow().base_handle.bounds();
    if bounds.size.height == Pixels::ZERO {
        return None;
    }
    let right_edge_x = bounds.origin.x + bounds.size.width;
    let row_top_y = bounds.origin.y + ROW_HEIGHT * row_index as f32 - view.tree_scroll_offset();
    Some(point(right_edge_x, row_top_y))
}

/// Close the open context menu, if any.
pub(super) fn close(view: &mut SidebarView, cx: &mut Context<SidebarView>) {
    if view.context_menu.take().is_some() {
        cx.notify();
    }
}

/// Write the open context menu's relation's bare name to the system
/// clipboard, then close the menu. A no-op if no menu is open.
pub(super) fn copy_name(view: &mut SidebarView, cx: &mut Context<SidebarView>) {
    if let Some(menu) = &view.context_menu {
        cx.write_to_clipboard(ClipboardItem::new_string(menu.relation.clone()));
    }
    close(view, cx);
}

/// Write the open context menu's relation's qualified `schema.relation`
/// name to the system clipboard, then close the menu. A no-op if no menu is
/// open.
pub(super) fn copy_qualified_name(view: &mut SidebarView, cx: &mut Context<SidebarView>) {
    if let Some(menu) = &view.context_menu {
        let qualified = qualified_relation_name(&menu.schema, &menu.relation);
        cx.write_to_clipboard(ClipboardItem::new_string(qualified));
    }
    close(view, cx);
}

/// `schema.relation`, the text `Copy Qualified Name` writes to the
/// clipboard.
pub(super) fn qualified_relation_name(schema: &str, relation: &str) -> String {
    format!("{schema}.{relation}")
}

/// The right-click context menu overlay: `Preview Data`, `View Schema`, a
/// separator, then `Copy Name`/`Copy Qualified Name`, anchored to the right
/// edge of its triggering relation row. A full-window backdrop behind it
/// absorbs off-menu clicks so closing the menu never doubles as activating
/// whatever sits beneath it. Renders nothing when no menu is open.
pub(super) fn render(view: &SidebarView, cx: &Context<SidebarView>) -> Option<gpui::AnyElement> {
    let menu = view.context_menu.clone()?;
    let schema = menu.schema.clone();
    let relation = menu.relation.clone();
    let kind = menu.kind;
    let anchor = relation_row_anchor(view, menu.row_index).unwrap_or(menu.fallback_position);

    let preview_schema = schema.clone();
    let preview_relation = relation.clone();
    let view_schema_schema = schema.clone();
    let view_schema_relation = relation.clone();

    let menu = ContextMenu::new("sidebar-context-menu")
        .position(anchor)
        .on_close(cx.listener(|view, _event, _window, cx| {
            close(view, cx);
        }))
        .add_item(ContextMenuItem::new("Preview Data").on_click(cx.listener(
            move |view, _event, window, cx| {
                view.preview(&preview_schema, &preview_relation, window, cx);
                close(view, cx);
            },
        )))
        .add_item(ContextMenuItem::new("View Schema").on_click(cx.listener(
            move |view, _event, _window, cx| {
                view.view_schema(&view_schema_schema, &view_schema_relation, kind, cx);
                close(view, cx);
            },
        )))
        .add_separator()
        .add_item(ContextMenuItem::new("Copy Name").on_click(cx.listener(
            |view, _event, _window, cx| {
                copy_name(view, cx);
            },
        )))
        .add_item(
            ContextMenuItem::new("Copy Qualified Name").on_click(cx.listener(
                |view, _event, _window, cx| {
                    copy_qualified_name(view, cx);
                },
            )),
        );

    Some(menu.into_any_element())
}
