//! [`WorkspaceStartup`] and the small entity-construction helpers

use std::path::PathBuf;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity};

use super::WorkspaceView;
use crate::connections::ConnectionStore;
use crate::session::Session;
use crate::ui::appearance::AppearanceModalView;
use crate::ui::connections::ConnectionManagerView;
use crate::ui::footer::ConnectionFooterView;
use crate::ui::results::ResultsView;
use crate::ui::sidebar::SidebarView;
use crate::ui::tabs::TabModel;

/// The persisted, path-shaped settings
pub struct WorkspaceStartup {
    /// Root directory per-connection tab sessions are read from and saved
    /// to (typically [`crate::config::Config::sessions_dir`]). `None`
    /// disables tab-session persistence entirely.
    pub sessions_root: Option<PathBuf>,
    /// Root directory the shared library's flat pool of `.sql` files lives
    /// under (typically [`crate::config::Config::library_dir`]). `None`
    /// disables saving to (or restoring from) the library entirely.
    pub library_root: Option<PathBuf>,
    /// The theme name the Appearance modal starts with its matching card
    /// checked/active (typically `cfg.theme.name`).
    pub active_theme_name: String,
    /// Where the Appearance modal discovers user theme files (typically
    /// [`crate::config::Config::themes_dir`]).
    pub themes_dir: Option<PathBuf>,
    /// Where the Appearance modal persists a selected theme name (typically
    /// [`crate::config::Config::default_path`]). `None` disables persistence
    /// for the session.
    pub config_path: Option<PathBuf>,
    /// How long the footer's post-save confirmation stays visible before
    /// clearing itself (typically
    /// [`crate::config::StatusConfig::save_confirmation_duration`]).
    pub save_confirmation_duration: Duration,
    /// Debounce interval a tab's autosave/draft write waits after an edit
    /// past its first (typically
    /// [`crate::config::AutosaveConfig::edit_debounce`]).
    pub edit_debounce: Duration,
    /// How often the sidebar's Scripts pane recomputes every row's
    /// relative-modified-time label (typically
    /// [`crate::config::SidebarConfig::scripts_relative_time_refresh`]).
    pub scripts_relative_time_refresh: Duration,
    /// The keystroke(s) that open the sidebar's find row, resolved from
    /// config the same way the sidebar's own binding is (typically
    /// `crate::keybindings::resolve`'s result's `sidebar.open_find`).
    pub open_find_keystrokes: Vec<String>,
}

impl Default for WorkspaceStartup {
    fn default() -> Self {
        Self {
            sessions_root: None,
            library_root: None,
            active_theme_name: zsql_ui::theme::ZSQL_DARK_NAME.to_owned(),
            themes_dir: None,
            config_path: None,
            save_confirmation_duration: crate::config::StatusConfig::default()
                .save_confirmation_duration(),
            edit_debounce: crate::config::AutosaveConfig::default().edit_debounce(),
            scripts_relative_time_refresh: crate::config::SidebarConfig::default()
                .scripts_relative_time_refresh(),
            open_find_keystrokes: crate::ui::sidebar::SidebarBindings::default().open_find,
        }
    }
}

impl WorkspaceView {
    /// Build the tab model over `session`, seeded with `library_root` and
    /// `edit_debounce` (typically
    /// [`crate::config::AutosaveConfig::edit_debounce`]).
    pub(super) fn build_tabs(
        session: &Entity<Session>,
        library_root: Option<PathBuf>,
        edit_debounce: Duration,
        claims: crate::session_store::SaveClaimFactory,
        cx: &mut Context<Self>,
    ) -> Entity<TabModel> {
        let tabs = cx.new(|cx| TabModel::new(session.clone(), cx));
        tabs.update(cx, |tabs, _cx| {
            tabs.set_library_dir(library_root);
            tabs.set_edit_debounce(edit_debounce);
            tabs.set_claim_factory(claims);
        });
        tabs
    }

    /// Build the connection manager, Appearance modal, and footer
    // Every parameter is an independent, already-resolved piece of state
    // these three entities need at construction
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_connection_chrome(
        session: Entity<Session>,
        results: &Entity<ResultsView>,
        connection_store: ConnectionStore,
        probe_timeout: Duration,
        batch_size: usize,
        active_theme_name: String,
        themes_dir: Option<PathBuf>,
        config_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> (
        Entity<ConnectionManagerView>,
        Entity<AppearanceModalView>,
        Entity<ConnectionFooterView>,
    ) {
        let connections = cx.new(|cx| {
            ConnectionManagerView::new(
                session.clone(),
                connection_store,
                probe_timeout,
                batch_size,
                cx,
            )
        });
        results.update(cx, |results, _cx| {
            results.set_connections_modal(connections.clone());
        });
        let appearance =
            cx.new(|cx| AppearanceModalView::new(active_theme_name, themes_dir, config_path, cx));
        let footer = cx.new(|cx| {
            ConnectionFooterView::new(session, connections.clone(), appearance.clone(), cx)
        });
        (connections, appearance, footer)
    }

    /// Build the schema sidebar over `session`/`tabs`, seeded with
    /// `library_root` and configured to recompute the Scripts pane's
    /// relative-time labels every `scripts_relative_time_refresh`
    pub(super) fn build_sidebar(
        session: &Entity<Session>,
        tabs: &Entity<TabModel>,
        library_root: Option<PathBuf>,
        scripts_relative_time_refresh: Duration,
        cx: &mut Context<Self>,
    ) -> Entity<SidebarView> {
        let sidebar =
            cx.new(|cx| SidebarView::new(session.clone(), tabs.clone(), library_root, cx));
        sidebar.update(cx, |sidebar, cx| {
            sidebar.set_scripts_refresh_interval(scripts_relative_time_refresh, cx);
        });
        sidebar
    }
}
