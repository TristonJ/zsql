//! Save/Save-as/Rename orchestration

use std::path::PathBuf;

use gpui::{AppContext as _, Context};

use super::WorkspaceView;
use crate::session_store::library::LibraryDir;
use crate::session_store::{self, ScriptBacking, SessionDir, SessionIo};
use crate::ui::connections::UNSAVED_CONNECTION_LABEL;
use crate::ui::footer::ConnectionFooterView;
use crate::ui::save_modal::{Destination, SQL_SUFFIX, SaveModalEvent, SaveModalKind};
use crate::ui::tabs::{SaveRequested, TabId};

/// `title` with a trailing `.sql` (if any) removed
fn strip_sql_suffix(title: &str) -> String {
    title.strip_suffix(SQL_SUFFIX).unwrap_or(title).to_owned()
}

impl WorkspaceView {
    pub(super) fn handle_save_requested(&mut self, evt: &SaveRequested, cx: &mut Context<Self>) {
        match *evt {
            SaveRequested::OpenSaveModal { tab_id, as_copy } => {
                self.open_save_modal(tab_id, as_copy, cx);
            }
            SaveRequested::WriteLibraryDirect { tab_id }
            | SaveRequested::WriteExternalDirect { tab_id } => {
                self.write_backing_direct(tab_id, cx);
            }
        }
    }

    pub(super) fn handle_save_modal_event(&mut self, evt: &SaveModalEvent, cx: &mut Context<Self>) {
        match evt {
            SaveModalEvent::Cancelled => {
                self.refocus_editor_on_next_render = true;
                cx.notify();
            }
            SaveModalEvent::Confirmed {
                tab_id,
                kind,
                name,
                destination,
            } => {
                self.perform_confirmed_save(*tab_id, *kind, name.clone(), *destination, cx);
            }
        }
    }

    /// The Save modal, restricted to the name field and path preview (no
    /// destination rows), for renaming `tab_id`'s underlying file in place.
    /// A no-op if `tab_id` is not a `Script` tab.
    pub fn open_rename_modal(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(backing) = self.tabs.read(cx).script_backing_of(tab_id) else {
            return;
        };
        let title = self
            .tabs
            .read(cx)
            .tab_title_of(tab_id)
            .unwrap_or_default()
            .to_owned();
        let (initial_name, destination) = match &backing {
            ScriptBacking::Library { name, .. } => (name.as_str().to_owned(), Destination::Library),
            ScriptBacking::SessionNamed { .. } | ScriptBacking::SessionScratch { .. } => {
                (strip_sql_suffix(&title), Destination::Connection)
            }
            ScriptBacking::External { .. } => return,
        };
        debug_assert!(backing.supports_rename(), "guarded by the match above");
        if destination == Destination::Connection
            && self.session_store.active_session_dir().is_none()
        {
            self.show_save_error("No active session to save into", cx);
            return;
        }
        let session_dir = self.session_dir_or_fallback();
        let library_dir = self.library_dir_or_fallback();
        let current_path = match &backing {
            ScriptBacking::SessionNamed { .. } | ScriptBacking::Library { .. } => {
                self.backing_disk_path(&backing)
            }
            ScriptBacking::SessionScratch { .. } | ScriptBacking::External { .. } => None,
        };
        let connection_name = self.active_connection_label(cx);
        self.save_modal.update(cx, |modal, cx| {
            modal.open(
                tab_id,
                SaveModalKind::Rename,
                &initial_name,
                destination,
                connection_name,
                session_dir,
                library_dir,
                current_path,
                cx,
            );
        });
        cx.notify();
    }

    /// The on-disk path `backing` lives at, or `None` when the directory
    /// that would anchor it (session or library) is unresolved.
    fn backing_disk_path(&self, backing: &ScriptBacking) -> Option<PathBuf> {
        let session_dir = match backing {
            ScriptBacking::SessionScratch { .. } | ScriptBacking::SessionNamed { .. } => {
                self.session_store.active_session_dir()?
            }
            ScriptBacking::Library { .. } | ScriptBacking::External { .. } => PathBuf::new(),
        };
        let library_dir = match backing {
            ScriptBacking::Library { .. } => self.library_dir.clone()?,
            _ => PathBuf::new(),
        };
        Some(backing.disk_path(&SessionDir::at(&session_dir), &LibraryDir::new(library_dir)))
    }

    /// The active connection's display name
    fn active_connection_label(&self, cx: &Context<Self>) -> String {
        self.connections.read(cx).active().map_or_else(
            || UNSAVED_CONNECTION_LABEL.to_owned(),
            |active| active.name.clone(),
        )
    }

    fn session_dir_or_fallback(&self) -> PathBuf {
        self.session_store
            .active_session_dir()
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn library_dir_or_fallback(&self) -> PathBuf {
        self.library_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn open_save_modal(&mut self, tab_id: TabId, as_copy: bool, cx: &mut Context<Self>) {
        let Some(backing) = self.tabs.read(cx).script_backing_of(tab_id) else {
            return;
        };
        let kind = if as_copy {
            SaveModalKind::SaveAs
        } else {
            SaveModalKind::Save
        };
        let (initial_name, initial_destination) = match &backing {
            ScriptBacking::Library { name, .. } => (name.as_str().to_owned(), Destination::Library),
            ScriptBacking::SessionNamed { .. } => {
                let title = self
                    .tabs
                    .read(cx)
                    .tab_title_of(tab_id)
                    .unwrap_or_default()
                    .to_owned();
                (strip_sql_suffix(&title), Destination::Connection)
            }
            ScriptBacking::SessionScratch { .. } => (String::new(), Destination::Connection),
            // An external tab's own Save is a direct write (see
            // `TabModel::request_save`); this modal only opens for its
            // Save-as, which starts from the tab's file name and the
            // ordinary "This connection" default.
            ScriptBacking::External { path, .. } => (
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                Destination::Connection,
            ),
        };
        let session_dir = self.session_dir_or_fallback();
        let library_dir = self.library_dir_or_fallback();
        let connection_name = self.active_connection_label(cx);
        self.save_modal.update(cx, |modal, cx| {
            modal.open(
                tab_id,
                kind,
                &initial_name,
                initial_destination,
                connection_name,
                session_dir,
                library_dir,
                None,
                cx,
            );
        });
        cx.notify();
    }

    /// Write a library- or external-backed tab's file directly with its
    /// current buffer text, no modal
    #[tracing::instrument(name = "workspace_write_backing_direct", skip(self, cx))]
    fn write_backing_direct(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(backing) = self.tabs.read(cx).script_backing_of(tab_id) else {
            return;
        };
        let Some(text) = self.tabs.read(cx).tab_buffer_text(tab_id, cx) else {
            return;
        };
        if matches!(backing, ScriptBacking::Library { .. }) && self.library_dir.is_none() {
            return;
        }
        let library_dir = self.library_dir_or_fallback();
        let tabs = self.tabs.clone();
        let file_label = backing_file_label(&backing);
        let session_dir = self.session_dir_or_fallback();

        cx.spawn(async move |this, cx| {
            let write_backing = backing.clone();
            let write_text = text.clone();
            let result = cx
                .background_spawn(async move {
                    let dir = SessionDir::at(&session_dir);
                    let library = LibraryDir::new(library_dir);
                    let io = SessionIo {
                        dir: &dir,
                        library: &library,
                    };
                    write_backing.write(&write_text, &io)
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(error = %err, "failed to save script");
                let _ = this.update(cx, |this, cx| {
                    this.show_save_error(&format!("Failed to save {file_label}"), cx);
                });
                return;
            }
            let _ = tabs.update(cx, |tabs, cx| tabs.mark_backing_saved(tab_id, text, cx));
            let _ = this.update(cx, |this, cx| this.show_save_confirmation(&file_label, cx));
        })
        .detach();
    }

    fn perform_confirmed_save(
        &mut self,
        tab_id: TabId,
        kind: SaveModalKind,
        name: String,
        destination: Destination,
        cx: &mut Context<Self>,
    ) {
        match kind {
            // Only ever requested for a scratch-backed session tab (see
            // `TabModel::request_save`): a named session tab is a no-op
            // and a library/external-backed one writes directly, with no
            // modal in either case.
            SaveModalKind::Save => self.perform_save(tab_id, name, destination, cx),
            // Always exports a copy and never retargets the source tab,
            // regardless of its own backing.
            SaveModalKind::SaveAs => self.perform_save_as(tab_id, &name, destination, cx),
            SaveModalKind::Rename => self.perform_rename(tab_id, name, cx),
        }
    }

    /// Save a scratch-backed session tab to `destination`: promote its file
    /// to the session directory's top level (Connection), write the library
    /// file and drop its former session file (Library), or export a copy
    /// with no retarget
    #[tracing::instrument(name = "workspace_perform_save", skip(self, cx))]
    fn perform_save(
        &mut self,
        tab_id: TabId,
        name: String,
        destination: Destination,
        cx: &mut Context<Self>,
    ) {
        if destination == Destination::External {
            self.perform_export_external(tab_id, &name, cx);
            return;
        }
        let Some(text) = self.tabs.read(cx).tab_buffer_text(tab_id, cx) else {
            return;
        };
        let Some(session_dir) = self.session_store.active_session_dir() else {
            // Reachable if the active connection's session directory could
            // not be resolved (e.g. no data directory) between the modal
            // opening and confirming
            self.show_save_error("No active session to save into", cx);
            return;
        };
        let Some(backing) = self.tabs.read(cx).script_backing_of(tab_id) else {
            return;
        };
        let file_label = format!("{name}{SQL_SUFFIX}");

        match destination {
            Destination::Connection => {
                self.spawn_session_rename(tab_id, backing, session_dir, file_label, "save", cx);
            }
            Destination::Library => {
                self.perform_save_to_library(
                    tab_id,
                    (name, text),
                    backing,
                    session_dir,
                    file_label,
                    cx,
                );
            }
            Destination::External => unreachable!("handled by the early return above"),
        }
    }

    /// Rename `backing`'s session file to `file_label`, retitling the tab on
    /// success.
    fn spawn_session_rename(
        &mut self,
        tab_id: TabId,
        backing: ScriptBacking,
        session_dir: PathBuf,
        file_label: String,
        error_verb: &'static str,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.tabs.clone();
        let new_title = file_label.clone();
        let library_dir = self.library_dir_or_fallback();
        let claim = self.session_store.claim_factory().mint(&session_dir);
        let new_file_name = file_label.clone();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let new_file = crate::session_store::ScriptFileName::new(new_file_name)
                        .map_err(|err| {
                            session_store::SessionStoreError::UnsafeRef(err.to_string())
                        })?;
                    let dir = SessionDir::at(&session_dir);
                    let library = LibraryDir::new(library_dir);
                    let io = SessionIo {
                        dir: &dir,
                        library: &library,
                    };
                    backing.rename(&new_file, claim, &io)
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(error = %err, "failed to rename session script");
                let _ = this.update(cx, |this, cx| {
                    this.show_save_error(&format!("Failed to {error_verb} {file_label}"), cx);
                });
                return;
            }
            let _ = tabs.update(cx, |tabs, cx| {
                tabs.apply_renamed_title(tab_id, new_title, cx);
            });
            let _ = this.update(cx, |this, cx| this.show_save_confirmation(&file_label, cx));
        })
        .detach();
    }

    fn perform_save_to_library(
        &mut self,
        tab_id: TabId,
        (name, text): (String, String),
        backing: ScriptBacking,
        session_dir: PathBuf,
        file_label: String,
        cx: &mut Context<Self>,
    ) {
        let Some(library_dir) = self.library_dir.clone() else {
            return;
        };
        let Ok(library_name) = session_store::LibraryName::new(name.clone()) else {
            self.show_save_error(&format!("Failed to save {file_label}"), cx);
            return;
        };
        let tabs = self.tabs.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn({
                    let library_dir = library_dir.clone();
                    let library_name = library_name.clone();
                    let text = text.clone();
                    async move { LibraryDir::new(library_dir).save(&library_name, &text) }
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(error = %err, "failed to save library script");
                let _ = this.update(cx, |this, cx| {
                    this.show_save_error(&format!("Failed to save {file_label}"), cx);
                });
                return;
            }

            if let ScriptBacking::SessionScratch { file } = &backing {
                let file = file.clone();
                let delete_result = cx
                    .background_spawn({
                        let session_dir = session_dir.clone();
                        async move { SessionDir::at(&session_dir).delete_scratch(&file) }
                    })
                    .await;
                if let Err(err) = delete_result {
                    tracing::warn!(
                        error = %err,
                        "saved to library but failed to delete the former session file"
                    );
                }
            }
            let _ = tabs.update(cx, |tabs, cx| {
                tabs.convert_to_library_backed(tab_id, name, text, cx);
            });
            let _ = this.update(cx, |this, cx| {
                this.show_save_confirmation(&file_label, cx);
                this.resync_sidebar_scripts(cx);
            });
        })
        .detach();
    }

    /// Export a copy of `tab_id`'s current buffer to `destination`
    #[tracing::instrument(name = "workspace_perform_save_as", skip(self, cx))]
    fn perform_save_as(
        &mut self,
        tab_id: TabId,
        name: &str,
        destination: Destination,
        cx: &mut Context<Self>,
    ) {
        if destination == Destination::External {
            self.perform_export_external(tab_id, name, cx);
            return;
        }
        let Some(text) = self.tabs.read(cx).tab_buffer_text(tab_id, cx) else {
            return;
        };
        let file_label = format!("{name}{SQL_SUFFIX}");

        match destination {
            Destination::Connection => {
                self.tabs.update(cx, |tabs, cx| {
                    tabs.new_script_tab_with_content(file_label.clone(), &text, cx);
                });
                self.dispatch_tab_session_save(cx);
                self.show_save_confirmation(&file_label, cx);
            }
            Destination::Library => {
                let Some(library_dir) = self.library_dir.clone() else {
                    return;
                };
                let Ok(library_name) = session_store::LibraryName::new(name) else {
                    self.show_save_error(&format!("Failed to save {file_label}"), cx);
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn({
                            let library_dir = library_dir.clone();
                            let library_name = library_name.clone();
                            let text = text.clone();
                            async move { LibraryDir::new(library_dir).save(&library_name, &text) }
                        })
                        .await;
                    if let Err(err) = result {
                        tracing::warn!(error = %err, "failed to export a library script copy");
                        let _ = this.update(cx, |this, cx| {
                            this.show_save_error(&format!("Failed to save {file_label}"), cx);
                        });
                        return;
                    }
                    let _ = this.update(cx, |this, cx| {
                        this.show_save_confirmation(&file_label, cx);
                        this.resync_sidebar_scripts(cx);
                    });
                })
                .detach();
            }
            Destination::External => unreachable!("handled by the early return above"),
        }
    }

    /// Export a copy of `tab_id`'s current buffer to a user-chosen path via
    /// the platform save-file dialog
    #[tracing::instrument(name = "workspace_perform_export_external", skip(self, cx))]
    fn perform_export_external(&mut self, tab_id: TabId, name: &str, cx: &mut Context<Self>) {
        let Some(text) = self.tabs.read(cx).tab_buffer_text(tab_id, cx) else {
            return;
        };
        let suggested_name = format!("{name}{SQL_SUFFIX}");
        let start_dir =
            crate::config::Config::default_export_dir().unwrap_or_else(|| PathBuf::from("."));
        let task = (self.save_file_prompt)(cx, &start_dir, Some(&suggested_name));

        cx.spawn(async move |this, cx| {
            let Some(path) = task.await else {
                return;
            };
            let result = cx
                .background_spawn({
                    let path = path.clone();
                    let text = text.clone();
                    async move { session_store::external::save(&path, &text) }
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(error = %err, "failed to export script");
                let file_label = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_owned();
                let _ = this.update(cx, |this, cx| {
                    this.show_save_error(&format!("Failed to save {file_label}"), cx);
                });
                return;
            }
            let file_label = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            let _ = this.update(cx, |this, cx| this.show_save_confirmation(&file_label, cx));
        })
        .detach();
    }

    /// Atomically rename `tab_id`'s underlying file (session-scoped or
    /// library) to `name` and retitle the tab.
    #[tracing::instrument(name = "workspace_perform_rename", skip(self, cx))]
    fn perform_rename(&mut self, tab_id: TabId, name: String, cx: &mut Context<Self>) {
        let Some(backing) = self.tabs.read(cx).script_backing_of(tab_id) else {
            return;
        };
        let tabs = self.tabs.clone();
        let file_label = format!("{name}{SQL_SUFFIX}");

        if let ScriptBacking::Library { name: old_name, .. } = &backing {
            let old_name = old_name.clone();
            let Some(library_dir) = self.library_dir.clone() else {
                return;
            };
            let Ok(new_library_name) = session_store::LibraryName::new(name.clone()) else {
                self.show_save_error(&format!("Failed to rename to {file_label}"), cx);
                return;
            };
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn({
                        let library_dir = library_dir.clone();
                        let old_name = old_name.clone();
                        let new_name = new_library_name.clone();
                        async move { LibraryDir::new(library_dir).rename(&old_name, &new_name) }
                    })
                    .await;
                if let Err(err) = result {
                    tracing::warn!(error = %err, "failed to rename library script");
                    let _ = this.update(cx, |this, cx| {
                        this.show_save_error(&format!("Failed to rename to {file_label}"), cx);
                    });
                    return;
                }
                let _ = tabs.update(cx, |tabs, cx| {
                    tabs.apply_library_rename(tab_id, &name, cx);
                });
                let _ = this.update(cx, |this, cx| {
                    this.show_save_confirmation(&file_label, cx);
                    this.resync_sidebar_scripts(cx);
                });
            })
            .detach();
            return;
        }

        let Some(session_dir) = self.session_store.active_session_dir() else {
            self.show_save_error("No active session to save into", cx);
            return;
        };
        self.spawn_session_rename(tab_id, backing, session_dir, file_label, "rename to", cx);
    }

    /// Rebuild the sidebar's SCRIPTS/LIBRARY rows from disk right now
    pub(super) fn resync_sidebar_scripts(&mut self, cx: &mut Context<Self>) {
        self.sidebar
            .update(cx, crate::ui::sidebar::SidebarView::resync_scripts);
    }

    /// Show the footer's transient "saved <file>" confirmation and clear it
    /// again after [`WorkspaceView::save_confirmation_duration`], unless a
    /// newer confirmation has already superseded this one.
    pub(super) fn show_save_confirmation(&mut self, file_label: &str, cx: &mut Context<Self>) {
        let label = file_label.to_owned();
        self.show_transient_footer_status(
            move |footer, cx| footer.show_saved_confirmation(&label, cx),
            cx,
        );
    }

    /// Show the footer's transient save-failure message and clear it again
    /// after [`WorkspaceView::save_confirmation_duration`]
    pub(super) fn show_save_error(&mut self, reason: &str, cx: &mut Context<Self>) {
        let reason = reason.to_owned();
        self.show_transient_footer_status(
            move |footer, cx| footer.show_save_error(&reason, cx),
            cx,
        );
    }

    /// Apply a transient footer status and clear it again after
    /// [`WorkspaceView::save_confirmation_duration`], unless a newer status
    /// has superseded this one by then.
    fn show_transient_footer_status(
        &mut self,
        apply: impl FnOnce(&mut ConnectionFooterView, &mut Context<ConnectionFooterView>),
        cx: &mut Context<Self>,
    ) {
        let generation = self.save_confirmation_generation.get() + 1;
        self.save_confirmation_generation.set(generation);
        self.footer.update(cx, apply);

        let footer = self.footer.clone();
        let generation_cell = self.save_confirmation_generation.clone();
        let duration = self.save_confirmation_duration;
        cx.spawn(async move |_this, cx| {
            cx.background_executor().timer(duration).await;
            if generation_cell.get() != generation {
                return;
            }
            let _ = footer.update(cx, ConnectionFooterView::clear_saved_confirmation);
        })
        .detach();
    }

    /// Export a copy of `tab_id`'s current buffer to the library under its
    /// current title, applying the same unique-name counter-suffix
    /// collision rule session script names use (never reimplemented)
    #[tracing::instrument(name = "workspace_copy_tab_to_library", skip(self, cx))]
    pub(crate) fn copy_tab_to_library(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.tabs.read(cx).script_backing_of(tab_id).is_none() {
            return;
        }
        let Some(title) = self.tabs.read(cx).tab_title_of(tab_id).map(str::to_owned) else {
            return;
        };
        let Some(text) = self.tabs.read(cx).tab_buffer_text(tab_id, cx) else {
            return;
        };
        let Some(library_dir) = self.library_dir.clone() else {
            return;
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn({
                    let library_dir = library_dir.clone();
                    let title = title.clone();
                    let text = text.clone();
                    async move {
                        let library = LibraryDir::new(library_dir);
                        let library_name = library.unique_name(&title)?;
                        library
                            .save(&library_name, &text)
                            .map(|()| library_name.as_str().to_owned())
                    }
                })
                .await;
            match result {
                Ok(bare_name) => {
                    let file_label = format!("{bare_name}{SQL_SUFFIX}");
                    let _ = this.update(cx, |this, cx| {
                        this.show_save_confirmation(&file_label, cx);
                        this.resync_sidebar_scripts(cx);
                    });
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to copy script to library");
                    let _ = this.update(cx, |this, cx| {
                        this.show_save_error("Failed to copy to library", cx);
                    });
                }
            }
        })
        .detach();
    }

    /// Shell out to the platform file manager to reveal `tab_id`'s backing
    /// file
    #[tracing::instrument(name = "workspace_reveal_tab_in_files", skip(self, cx))]
    pub(crate) fn reveal_tab_in_files(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(backing) = self.tabs.read(cx).script_backing_of(tab_id) else {
            return;
        };
        let Some(path) = self.backing_disk_path(&backing) else {
            return;
        };

        cx.background_spawn(async move {
            if let Err(err) = crate::reveal::reveal_in_file_manager(&path) {
                tracing::warn!(error = %err, path = %path.display(), "failed to reveal in file manager");
            }
        })
        .detach();
    }
}

/// The label a save/write confirmation or error message shows for `backing`.
fn backing_file_label(backing: &ScriptBacking) -> String {
    match backing {
        ScriptBacking::SessionScratch { file } | ScriptBacking::SessionNamed { file } => {
            file.as_str().to_owned()
        }
        ScriptBacking::Library { name, .. } => format!("{}{SQL_SUFFIX}", name.as_str()),
        ScriptBacking::External { path, .. } => path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::session_store::{
        LibraryDir, LibraryName, ScriptBacking, ScriptFileName, SessionDir,
    };

    fn disk_path(backing: &ScriptBacking) -> PathBuf {
        backing.disk_path(
            &SessionDir::at(Path::new("/data/sessions/1f3a")),
            &LibraryDir::at(Path::new("/data/library")),
        )
    }

    #[test]
    fn disk_path_for_a_named_session_tab_joins_the_session_dir_and_file_name() {
        let backing = ScriptBacking::SessionNamed {
            file: ScriptFileName::new("top-customers.sql").unwrap(),
        };
        assert_eq!(
            disk_path(&backing),
            Path::new("/data/sessions/1f3a/scripts/top-customers.sql")
        );
    }

    #[test]
    fn disk_path_for_a_scratch_session_tab_resolves_its_scratch_sibling() {
        let backing = ScriptBacking::SessionScratch {
            file: ScriptFileName::new("query-1.sql").unwrap(),
        };
        assert_eq!(
            disk_path(&backing),
            Path::new("/data/sessions/1f3a/scratch/query-1.sql")
        );
    }

    #[test]
    fn disk_path_for_a_library_tab_joins_the_library_dir_and_name() {
        let backing = ScriptBacking::Library {
            name: LibraryName::new("revenue-report").unwrap(),
            saved_text: None,
        };
        assert_eq!(
            disk_path(&backing),
            Path::new("/data/library/revenue-report.sql")
        );
    }

    #[test]
    fn disk_path_for_an_external_tab_is_the_external_path_itself() {
        let backing = ScriptBacking::External {
            path: PathBuf::from("/home/t/reports/quarterly.sql"),
            saved_text: None,
        };
        assert_eq!(
            disk_path(&backing),
            Path::new("/home/t/reports/quarterly.sql")
        );
    }
}
