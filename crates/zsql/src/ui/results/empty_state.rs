//! The results pane's body for [`SessionState::Empty`][crate::session::SessionState::Empty]:
//! the copy shown before any connection has ever been established, plus the
//! control that opens the connection manager directly from an otherwise
//! idle results pane.

use gpui::{ClickEvent, Context, Div, Entity, Stateful, Window, prelude::*};
use zsql_ui::button::primary_button;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::theme::ActiveTheme;

use crate::ui::connections::ConnectionManagerView;
use crate::ui::theme;

use super::ResultsView;

/// The Empty-state title, shown above [`DETAIL`].
pub(super) const TITLE: &str = "Not connected";
/// The Empty-state subtitle, shown below [`TITLE`].
pub(super) const DETAIL: &str = "Add a connection to browse schemas and run queries.";

/// Element id and debug selector for [`render_add_connection_cta`], so
/// tests can locate the control's painted bounds.
pub(super) const ADD_CONNECTION_ID: &str = "results-add-connection";

/// The "Add connection" call to action shown below the Empty-state
/// subtitle. Clicking it opens (and focuses) `connections` -- the same
/// connection-manager modal instance the connection footer's own
/// click-to-connect affordance opens, never a second one. A no-op click
/// while `connections` is `None`, e.g. a [`ResultsView`] that has not gone
/// through [`ResultsView::set_connections_modal`].
pub(super) fn render_add_connection_cta(
    connections: Option<Entity<ConnectionManagerView>>,
    window: &mut Window,
    cx: &mut Context<ResultsView>,
) -> Stateful<Div> {
    let icon_color = cx.theme().colors.accent;

    primary_button(ADD_CONNECTION_ID, window, cx)
        .debug_selector(|| ADD_CONNECTION_ID.to_owned())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(icon(IconName::Add, theme::MODAL_ADD_ICON_SIZE, icon_color))
        .child("Add connection")
        .on_click(cx.listener(move |_view, _event: &ClickEvent, window, cx| {
            let Some(connections) = connections.as_ref() else {
                return;
            };
            let focus_handle = connections.read(cx).modal_focus_handle();
            connections.update(cx, ConnectionManagerView::open);
            window.focus(&focus_handle);
        }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, point, px};
    use zsql_core::ResultSet;

    use super::{ADD_CONNECTION_ID, DETAIL, TITLE};
    use crate::connections::ConnectionStore;
    use crate::session::{Session, SessionState};
    use crate::ui::connections::ConnectionManagerView;
    use crate::ui::results::ResultsView;

    fn empty_connection_store(label: &str) -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-results-empty-state-test-{label}-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    #[test]
    fn documented_copy_matches_the_required_strings() {
        assert_eq!(TITLE, "Not connected");
        assert_eq!(
            DETAIL,
            "Add a connection to browse schemas and run queries."
        );
    }

    #[gpui::test]
    fn the_add_connection_control_is_painted_with_a_stable_selector(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_render_test(SessionState::Empty, ResultSet::default()));
        let (_view, vcx) = cx.add_window_view(|_window, cx| ResultsView::new(session, "", cx));
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds(ADD_CONNECTION_ID).is_some(),
            "the Add connection control must be painted on the Empty-state body"
        );
    }

    #[gpui::test]
    fn clicking_add_connection_without_a_modal_wired_up_is_a_no_op(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_render_test(SessionState::Empty, ResultSet::default()));
        let (_view, vcx) = cx.add_window_view(|_window, cx| ResultsView::new(session, "", cx));
        vcx.run_until_parked();

        let bounds = vcx
            .debug_bounds(ADD_CONNECTION_ID)
            .expect("the Add connection control must be painted even with no modal wired up");
        let position = point(bounds.origin.x + px(5.0), bounds.origin.y + px(5.0));
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds(ADD_CONNECTION_ID).is_some(),
            "clicking the control with no connections modal wired up must be a safe no-op"
        );
    }

    #[gpui::test]
    fn clicking_add_connection_opens_the_shared_connections_modal(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_render_test(SessionState::Empty, ResultSet::default()));
        let connections = cx.new(|cx| {
            ConnectionManagerView::new(
                session.clone(),
                empty_connection_store("click"),
                Duration::from_millis(100),
                cx,
            )
        });
        let connections_for_view = connections.clone();
        let (_view, vcx) = cx.add_window_view(|_window, cx| {
            let mut view = ResultsView::new(session, "", cx);
            view.set_connections_modal(connections_for_view);
            view
        });
        vcx.run_until_parked();

        assert!(
            !connections.read_with(vcx, |c, _app| c.is_open()),
            "the connections modal must start closed"
        );

        let bounds = vcx
            .debug_bounds(ADD_CONNECTION_ID)
            .expect("the Add connection control must be painted");
        let position = point(bounds.origin.x + px(5.0), bounds.origin.y + px(5.0));
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        assert!(
            connections.read_with(vcx, |c, _app| c.is_open()),
            "clicking Add connection must open the same connections modal the footer opens"
        );

        let focus_handle = connections.read_with(vcx, |c, _app| c.modal_focus_handle());
        vcx.update(|window, _cx| {
            assert!(
                focus_handle.is_focused(window),
                "clicking Add connection must also focus the connections modal, \
                 matching the footer's open-and-focus contract"
            );
        });
    }
}
