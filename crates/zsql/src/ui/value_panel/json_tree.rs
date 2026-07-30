//! The value panel's JSON tree renderer and its keyboard navigation
//! (up/down/collapse/expand).

use gpui::{ClickEvent, Context, SharedString, Stateful, Window, div, prelude::*, rgb};
use zsql_ui::theme::Theme;
use zsql_ui::tree::{disclosure_glyph, disclosure_spacer, row_meta, row_shell};

use super::data::{self, JsonLoad, JsonNode, PathSegment};
use super::view::{TreeCollapse, TreeDown, TreeExpand, TreeUp, ValuePanel};
use crate::ui::format::ValueKind;
use crate::ui::theme;

impl ValuePanel {
    pub(super) fn render_json_tree(
        &self,
        root: &JsonNode,
        active_theme: &Theme,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        let rows = data::visible_tree_rows(root, self.state.tree_expanded());
        let selected_path = self.state.selected_tree_path().to_vec();

        let mut list = div()
            .id("value-panel-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        for row in rows {
            let Some(target) = data::node_at_path(root, &row.path) else {
                continue;
            };
            list = list.child(self.render_json_tree_row(
                &row,
                target,
                &selected_path,
                active_theme,
                cx,
            ));
        }
        list
    }

    fn render_json_tree_row(
        &self,
        row: &data::TreeRow,
        target: &JsonNode,
        selected_path: &[PathSegment],
        active_theme: &Theme,
        cx: &Context<Self>,
    ) -> Stateful<gpui::Div> {
        let colors = active_theme.colors;
        let is_selected = row.path == selected_path;
        let has_children = matches!(target, JsonNode::Object(_) | JsonNode::Array(_));
        let expanded = self.state.is_tree_node_expanded(&row.path);
        let indent = tree_row_indent(row.depth);
        let row_id = data::json_path_string(&row.path);

        let key_label = row.path.last().map(|segment| match segment {
            PathSegment::Key(key) => key.clone(),
            PathSegment::Index(index) => index.to_string(),
        });

        let mut shell = row_shell(indent, active_theme).id(SharedString::from(row_id));

        // Clicking a row selects it; clicking a row that can be disclosed
        // also toggles its expansion, so the disclosure glyph and the row's
        // label/value both act as one affordance.
        let path_for_click = row.path.clone();
        shell = shell.cursor_pointer().on_click(cx.listener(
            move |view, _event: &ClickEvent, _window, cx| {
                view.state.select_tree_path(path_for_click.clone());
                if has_children {
                    view.state.toggle_tree_node(&path_for_click);
                }
                cx.notify();
            },
        ));

        if has_children {
            shell = shell.child(disclosure_glyph(expanded, active_theme));
        } else {
            shell = shell.child(disclosure_spacer());
        }

        if let Some(key) = key_label {
            shell = shell.child(div().text_color(rgb(colors.text_secondary)).child(key));
        }

        match target {
            JsonNode::Object(_) | JsonNode::Array(_) => {}
            other => {
                let kind = data::node_value_kind(other).unwrap_or(ValueKind::Unknown);
                shell = shell.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(kind.color(active_theme)))
                        .child(json_scalar_text(other)),
                );
            }
        }

        if let Some(count_label) = data::child_count_label(target) {
            shell = shell.child(row_meta(count_label, active_theme));
        }

        if is_selected {
            shell = shell
                .bg(theme::sidebar_selected_bg(active_theme))
                .border_l_2()
                .border_color(rgb(colors.accent));
        }

        shell
    }

    // ---- value panel: JSON tree keyboard navigation -------------------

    pub(super) fn tree_up(&mut self, _: &TreeUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_tree_selection(-1, cx);
    }

    pub(super) fn tree_down(&mut self, _: &TreeDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_tree_selection(1, cx);
    }

    fn move_tree_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(JsonLoad::Parsed(root)) = self.json.as_ref().map(|c| &c.load) else {
            return;
        };
        let rows = data::visible_tree_rows(root, self.state.tree_expanded());
        let current = self.state.selected_tree_path().to_vec();
        let next = data::move_tree_selection(&rows, &current, delta);
        self.state.select_tree_path(next);
        cx.notify();
    }

    pub(super) fn tree_collapse(
        &mut self,
        _: &TreeCollapse,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.state.selected_tree_path().to_vec();
        self.state.set_tree_node_expanded(path, false);
        cx.notify();
    }

    pub(super) fn tree_expand(
        &mut self,
        _: &TreeExpand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(JsonLoad::Parsed(root)) = self.json.as_ref().map(|c| &c.load) else {
            return;
        };
        let path = self.state.selected_tree_path().to_vec();
        if let Some(node) = data::node_at_path(root, &path)
            && matches!(node, JsonNode::Object(_) | JsonNode::Array(_))
        {
            self.state.set_tree_node_expanded(path, true);
            cx.notify();
        }
    }
}

/// Left indent (px) for a JSON tree row at `depth`, growing linearly with
/// nesting the same way the sidebar's schema tree does.
#[allow(clippy::cast_precision_loss)]
fn tree_row_indent(depth: usize) -> f32 {
    theme::VALUE_PANEL_TREE_INDENT * depth as f32
}

/// A JSON tree scalar's display text: a quoted string, or the number/bool/
/// null token as-is. Objects/arrays have no scalar text of their own (their
/// row shows a child-count label instead; see
/// [`data::child_count_label`]).
fn json_scalar_text(node: &JsonNode) -> String {
    match node {
        JsonNode::String(s) => format!("\"{s}\""),
        JsonNode::Number(n) => n.clone(),
        JsonNode::Bool(b) => b.to_string(),
        JsonNode::Null => "null".to_owned(),
        JsonNode::Object(_) | JsonNode::Array(_) => String::new(),
    }
}
