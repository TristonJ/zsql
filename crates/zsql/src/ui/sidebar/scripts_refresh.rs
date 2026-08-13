//! The Scripts pane's periodic relative-time refresh loop

use std::time::{Duration, SystemTime};

use gpui::{AppContext as _, Context, Task};

use super::SidebarView;
use super::model::{SessionScript, SidebarPane};
use crate::session_store::{self, SessionDir};
use crate::ui::open_modal::LibraryScript;
use crate::ui::time_fmt;

impl SidebarView {
    /// Override [`SidebarView::scripts_refresh_interval`] after construction.
    /// Respawns the loop so an iteration already sleeping on the old interval
    /// cannot swallow the override.
    pub(crate) fn set_scripts_refresh_interval(
        &mut self,
        interval: Duration,
        cx: &mut Context<Self>,
    ) {
        self.scripts_refresh_interval = interval;
        self.spawn_scripts_refresh_loop(cx);
    }

    /// Spawn a self-rescheduling loop that recomputes every script row's
    /// relative-modified-time label every
    /// [`SidebarView::scripts_refresh_interval`] for as long as this view
    /// exists. Replaces (and thereby cancels) any previously spawned loop.
    pub(super) fn spawn_scripts_refresh_loop(&mut self, cx: &mut Context<Self>) {
        self.scripts_refresh_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |view, _cx| view.scripts_refresh_interval)
                else {
                    return;
                };
                cx.background_executor().timer(interval).await;

                let Ok(on_scripts_pane) =
                    this.update(cx, |view, _cx| view.active_pane == SidebarPane::Scripts)
                else {
                    return;
                };
                if !on_scripts_pane {
                    continue;
                }

                let Ok((refresh, generation)) = this.update(cx, |view, cx| {
                    (
                        view.spawn_script_rows_refresh(cx),
                        view.script_rows_generation,
                    )
                }) else {
                    return;
                };
                let rows = refresh.await;

                let Ok(()) = this.update(cx, |view, cx| {
                    view.apply_background_script_rows(rows, generation, cx);
                }) else {
                    return;
                };
            }
        }));
    }

    /// Apply a completed background scan's listings: refresh the caches and
    /// rebuild the rows against the tabs' current open/active state.
    pub(super) fn apply_background_script_rows(
        &mut self,
        listings: (Vec<SessionScript>, Vec<LibraryScript>),
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.script_rows_generation != generation {
            tracing::debug!(
                captured = generation,
                current = self.script_rows_generation,
                "dropping a background script-rows refresh result superseded by a synchronous resync"
            );
            return;
        }
        (self.cached_session_scripts, self.cached_library_scripts) = listings;
        self.rebuild_script_rows_from_cache(cx);
        cx.notify();
    }

    fn spawn_script_rows_refresh(
        &self,
        cx: &mut Context<Self>,
    ) -> Task<(Vec<SessionScript>, Vec<LibraryScript>)> {
        let session_dir = self.session_dir.clone();
        let library_dir = self.library_dir.clone();
        cx.background_spawn(async move {
            let now = SystemTime::now();
            let session_scripts: Vec<SessionScript> = session_dir
                .as_ref()
                .and_then(|dir| SessionDir::at(dir).list_scripts().ok())
                .unwrap_or_default()
                .into_iter()
                .map(|entry| SessionScript {
                    file_name: entry.file_name,
                    relative_time: time_fmt::relative_time(now, entry.modified),
                })
                .collect();

            let library_scripts: Vec<LibraryScript> = library_dir
                .as_ref()
                .and_then(|dir| session_store::LibraryDir::at(dir).list().ok())
                .unwrap_or_default()
                .into_iter()
                .map(|entry| LibraryScript {
                    name: entry.name,
                    relative_time: time_fmt::relative_time(now, entry.modified),
                })
                .collect();

            (session_scripts, library_scripts)
        })
    }
}
