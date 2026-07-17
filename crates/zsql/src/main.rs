//! zsql -- a lightweight Postgres-first SQL editor (gpui).
//!
//! The window opens into `ui::workspace::WorkspaceView`, which lays out the
//! schema sidebar to the left of the results grid, both driven by a shared
//! `Session`. Startup resolves the configured DSN, connects, and
//! introspects the schema; the results grid stays idle (connected, no
//! query run yet) until the user clicks a relation in the sidebar. There is
//! no editor pane wired up yet -- that is a later addition.

mod config;
mod observability;
mod session;
mod sql;
mod ui;

use config::Config;
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

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                let session = cx.new(|_cx| Session::new(&cfg));

                // `WorkspaceView` builds its own sidebar/results entities
                // over this session (see `ui::workspace`), each of which
                // subscribes to it directly: nothing here has to re-derive
                // or push a snapshot of the session's state on each update.
                let workspace_session = session.clone();
                let workspace = cx.new(|cx| WorkspaceView::new(workspace_session, cx));

                // Connect, then introspect, on gpui's own executors (no
                // tokio runtime); the session updates itself (and, via the
                // sidebar/results views' own subscriptions, the UI) as each
                // step progresses. If `connect` finds no DSN configured, it
                // leaves the session in `SessionState::Empty` (the prompt
                // state) rather than erroring, and this skips introspecting
                // a connection that was never made rather than turning that
                // prompt into a fabricated error. No query is run here: the
                // results grid stays in its idle `Connected` state until
                // the user clicks a relation in the sidebar.
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
