//! zsql — a lightweight Postgres-first SQL editor (gpui).
//!
//! The window opens straight into the results grid (`ui::results::ResultsView`),
//! driven by a `Session` that resolves the configured DSN, connects, and runs
//! a hardcoded startup query. There is no editor pane wired up yet — that is
//! why the query is hardcoded rather than typed — but the connection, the
//! query stream, and the grid it feeds are all real and live.

mod config;
mod observability;
mod session;
mod ui;

use config::Config;
use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use session::{Session, SessionState};
use ui::results::ResultsView;

/// Default window size for the results-grid preview.
const WINDOW_WIDTH: f32 = 1180.0;
/// Default window size for the results-grid preview.
const WINDOW_HEIGHT: f32 = 760.0;

/// The query run automatically once startup connects. Hardcoded for now —
/// there is no editor pane yet to author SQL with — but it runs against a
/// live connection like any future user-typed query would.
const STARTUP_QUERY: &str = "SELECT * FROM orders ORDER BY placed_at DESC";

/// The results grid's source label while the startup query above is active.
const STARTUP_SOURCE_LABEL: &str = "public.orders";

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

                // `ResultsView` holds its own `Entity<Session>` and reads it
                // directly on every render (see `ui::results`'s module doc
                // comment): it subscribes to the session itself in its
                // constructor, so nothing here has to re-derive or clone a
                // snapshot of the session's state on each update.
                let session_for_view = session.clone();
                let results_view = cx.new(move |results_cx| {
                    ResultsView::new(session_for_view, STARTUP_SOURCE_LABEL, results_cx)
                });

                // Connect on startup, then run the hardcoded query once
                // connected. Both steps run on gpui's own executors (no
                // tokio runtime); the session updates itself (and, via
                // `ResultsView`'s own subscription, the grid) as each step
                // progresses. If `connect` finds no DSN configured, it
                // leaves the session in `SessionState::Empty` (the prompt
                // state) rather than erroring, and this skips running a
                // query against nothing rather than turning that prompt into
                // a fabricated error. If `connect` succeeds with nothing to
                // run yet, the session lands in `SessionState::Connected`
                // and stays there — this startup path always has a query to
                // run immediately after, so that state is transient here,
                // but a connect-without-query caller (e.g. a future "New
                // connection" flow with no query typed yet) would see it
                // rendered directly by `ResultsView`.
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
                        let run_task = startup_session
                            .update(cx, |session, cx| session.run_query(STARTUP_QUERY, cx))?;
                        run_task.await;
                    }

                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);

                results_view
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });

    Ok(())
}
