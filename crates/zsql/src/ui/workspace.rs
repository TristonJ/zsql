//! The root workspace view

use gpui::{Context, Entity, Render, Window, div, prelude::*, rgb};

use super::results::ResultsView;
use super::sidebar::SidebarView;
use super::theme;
use crate::session::Session;

pub struct WorkspaceView {
    sidebar: Entity<SidebarView>,
    results: Entity<ResultsView>,
}

impl WorkspaceView {
    /// Build a workspace over `session`
    #[must_use]
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let results = cx.new(|cx| ResultsView::new(session.clone(), "", cx));
        let sidebar = cx.new(|cx| SidebarView::new(session, results.clone(), cx));
        Self { sidebar, results }
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(theme::INK))
            .child(
                div()
                    .flex_shrink_0()
                    .w(theme::SIDEBAR_WIDTH)
                    .h_full()
                    .border_r_1()
                    .border_color(rgb(theme::LINE))
                    .child(self.sidebar.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.results.clone()),
            )
    }
}

#[cfg(test)]
mod render_tests {
    use gpui::AppContext as _;
    use zsql_core::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::WorkspaceView;
    use crate::session::{SchemaState, Session};

    #[gpui::test]
    fn renders_the_sidebar_and_results_grid_side_by_side_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let tree = SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![SchemaNs {
                    name: "public".to_owned(),
                    tables: vec![Relation {
                        name: "orders".to_owned(),
                        kind: RelationKind::Table,
                        columns: vec![],
                    }],
                }],
            }],
        };
        let session = cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(tree)));
        cx.add_window_view(|_window, cx| WorkspaceView::new(session, cx));
    }
}
