//! zsql -- a lightweight Postgres-first SQL editor (gpui).

mod config;
mod connections;
mod drivers;
mod observability;
mod session;
mod sql;
mod tab_session;
#[cfg(test)]
mod test_support;
mod ui;

use config::Config;
use connections::ConnectionStore;
use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use session::{Session, SessionState};
use ui::connections::active_connection_for_url;
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
    // Snapshot before `connection_store` is moved into `WorkspaceView::new`
    // below: the startup connect task still needs it to resolve the footer's
    // active-connection label for a `DATABASE_URL`/`Config`-fallback DSN
    // that matches (or doesn't match) a saved connection.
    let saved_connections = connection_store.connections().to_vec();
    let resolved_dsn = cfg.resolve_url();

    Application::new()
        .with_assets(zsql_ui::icon::IconAssetSource)
        .run(move |cx: &mut App| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);

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
                    let tab_sessions_path = Config::tab_sessions_path();
                    let workspace = cx.new(|cx| {
                        WorkspaceView::new(
                            workspace_session,
                            workspace_layout,
                            connection_store,
                            tab_sessions_path,
                            cx,
                        )
                    });
                    if let Some(handle) = workspace.read(cx).editor_focus_handle(cx) {
                        window.focus(&handle);
                    }

                    // Flush the active connection's tab session to disk on
                    // quit, so an edit made just before quitting is not lost
                    // to a fire-and-forget background write racing process
                    // exit.
                    let quit_workspace = workspace.clone();
                    cx.on_app_quit(move |cx| {
                        let task =
                            quit_workspace.update(cx, WorkspaceView::flush_tab_session_on_quit);
                        async move {
                            task.await;
                        }
                    })
                    .detach();

                    let startup_session = session.clone();
                    let startup_workspace = workspace.clone();
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
                            if let Some(dsn) = &resolved_dsn {
                                let active = active_connection_for_url(dsn, &saved_connections);
                                startup_workspace.update(cx, |workspace, cx| {
                                    workspace.set_active_connection(active, cx);
                                })?;
                            }
                            let introspect_task =
                                startup_session.update(cx, Session::introspect)?;
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
