//! zsql -- a lightweight Postgres-first SQL editor (gpui).

mod config;
mod connections;
#[cfg(all(test, feature = "driver-integration-tests"))]
mod database_switch_live_tests;
mod drivers;
mod keyring;
mod observability;
mod reveal;
mod session;
mod session_store;
#[cfg(all(test, feature = "ssh-integration-tests"))]
mod ssh_live_tests;
mod staging;
#[cfg(test)]
mod test_support;
mod theme_resolve;
mod ui;

use config::Config;
use connections::ConnectionStore;
use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*};
use session::{Session, SessionState};
use ui::workspace::{WorkspaceStartup, WorkspaceView};
use zsql_ui::theme::{Theme, get_builtin_fonts};

/// The window title shown by the OS (taskbar, window list, title bar).
const APP_TITLE: &str = "zsql";
/// Reverse-DNS application identifier reported to the OS. Desktop environments
/// use it to group the app's windows together, and it is the id the platform
/// packaging (macOS bundle, Linux `.desktop` `StartupWMClass`) must match.
const APP_ID: &str = "com.tristonj.zsql";

/// One-time migration of the legacy `tab_sessions.json` store into the new
/// per-connection session directories, if the legacy file is still present.
/// A no-op on every startup after the first successful migration.
fn migrate_legacy_sessions_if_present(connection_store: &ConnectionStore) {
    if let (Some(legacy_path), Some(sessions_root)) =
        (Config::tab_sessions_path(), Config::sessions_dir())
    {
        session_store::migration::migrate_legacy_sessions(
            &legacy_path,
            &sessions_root,
            connection_store.connections(),
        );
    }
}

fn build_workspace_window(
    window: &mut gpui::Window,
    cx: &mut App,
    cfg: &Config,
    connection_store: ConnectionStore,
) -> gpui::Entity<WorkspaceView> {
    let session = cx.new(|_cx| Session::new(cfg));

    let workspace_session = session.clone();
    let workspace_layout = cfg.layout.clone();
    let workspace_value_panel = cfg.value_panel.clone();
    let probe_timeout = cfg.liveness.probe_timeout();
    let batch_size = cfg.query.batch_size;
    let startup = WorkspaceStartup {
        sessions_root: Config::sessions_dir(),
        library_root: Config::library_dir(),
        active_theme_name: cfg.theme.name.clone(),
        themes_dir: Config::themes_dir(),
        config_path: Config::default_path(),
        save_confirmation_duration: cfg.status.save_confirmation_duration(),
        edit_debounce: cfg.autosave.edit_debounce(),
        scripts_relative_time_refresh: cfg.sidebar.scripts_relative_time_refresh(),
    };
    let workspace = cx.new(|cx| {
        WorkspaceView::new(
            workspace_session,
            workspace_layout,
            workspace_value_panel,
            connection_store,
            probe_timeout,
            batch_size,
            startup,
            cx,
        )
    });
    if let Some(handle) = workspace.read(cx).editor_focus_handle(cx) {
        window.focus(&handle);
    }

    // Flush the active connection's tab session to disk on quit, so an
    // edit made just before quitting is not lost to a fire-and-forget
    // background write racing process exit.
    let quit_workspace = workspace.clone();
    cx.on_app_quit(move |cx| {
        quit_workspace.update(cx, WorkspaceView::flush_theme_on_quit);
        let task = quit_workspace.update(cx, WorkspaceView::flush_tab_session_on_quit);
        async move {
            task.await;
        }
    })
    .detach();

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
}

fn main() -> anyhow::Result<()> {
    observability::init();

    let cfg = match Config::default_path() {
        Some(path) => Config::load_or_default(&path)?,
        None => Config::default(),
    };

    let connection_store = match Config::connections_path() {
        Some(path) => ConnectionStore::load(&path)?,
        None => ConnectionStore::in_memory(),
    };
    migrate_legacy_sessions_if_present(&connection_store);

    Application::new()
        .with_assets(zsql_ui::icon::IconAssetSource)
        .run(move |cx: &mut App| {
            let colors = theme_resolve::resolve(&cfg.theme.name, Config::themes_dir().as_deref());
            cx.set_global(Theme {
                colors,
                ..Default::default()
            });
            if let Err(e) = cx.text_system().add_fonts(get_builtin_fonts()) {
                tracing::error!("failed to register builtin fonts: {}", e);
            }
            let available_fonts = cx.text_system().all_font_names().join(",");
            tracing::debug!("all available fonts are: {}", available_fonts);

            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            ui::results::init(cx, &cfg.staging.apply_keybinding);
            ui::schema_view::init(cx);
            ui::save_modal::init(cx);
            ui::open_modal::init(cx);

            let bounds = Bounds::maximized(None, cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Maximized(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(APP_TITLE.into()),
                        ..Default::default()
                    }),
                    app_id: Some(APP_ID.to_owned()),
                    ..Default::default()
                },
                |window, cx| build_workspace_window(window, cx, &cfg, connection_store),
            )
            .expect("failed to open window");
            cx.activate(true);
        });

    Ok(())
}
