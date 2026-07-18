//! zsql -- a lightweight Postgres-first SQL editor (gpui).

mod config;
mod connections;
mod drivers;
mod observability;
mod session;
mod sql;
mod ui;

use config::Config;
use connections::ConnectionStore;
use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use session::{Session, SessionState};
use ui::workspace::WorkspaceView;

/// Default window size for the workspace.
const WINDOW_WIDTH: f32 = 1180.0;
/// Default window size for the workspace.
const WINDOW_HEIGHT: f32 = 760.0;

fn main() -> anyhow::Result<()> {
    observability::init();

    let cfg = match Config::default_path() {
        Some(path) => Config::load_or_default(&path)?,
        None => Config::default(),
    };
    let has_configured_url = cfg.resolve_url().is_some();
    tracing::info!(theme = %cfg.theme.name, has_configured_url, "zsql starting");

    let connection_store = match Config::connections_path() {
        Some(path) => ConnectionStore::load(&path)?,
        None => ConnectionStore::in_memory(),
    };

    Application::new().run(move |cx: &mut App| {
        zsql_editor::init(cx);

        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let session = cx.new(|_cx| Session::new(&cfg));

                let workspace_session = session.clone();
                let workspace_layout = cfg.layout.clone();
                let workspace = cx.new(|cx| {
                    WorkspaceView::new(workspace_session, workspace_layout, connection_store, cx)
                });
                window.focus(&workspace.read(cx).editor_focus_handle(cx));

                let startup_session = session.clone();
                cx.spawn(async move |cx| {
                    let connect_task = startup_session.update(cx, Session::connect)?;
                    connect_task.await;

                    let is_connected = startup_session.read_with(cx, |session, _app| {
                        !matches!(
                            session.state(),
                            SessionState::Empty | SessionState::Error(_)
                        )
                    })?;

                    if is_connected {
                        let introspect_task = startup_session.update(cx, Session::introspect)?;
                        introspect_task.await;
                    }

                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);

                workspace
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });

    Ok(())
}
