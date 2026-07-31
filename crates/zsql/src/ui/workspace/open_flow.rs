//! Open Script / Browse files orchestration

use std::cell::Cell;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::{AppContext as _, Context, Entity};

use super::WorkspaceView;
use crate::session_store::{self, SessionDir};
use crate::ui::connections::UNSAVED_CONNECTION_LABEL;
use crate::ui::open_modal::{
    LibraryScript, OpenModalEvent, OpenModalView, PickerTarget, SessionScript,
};
use crate::ui::tabs::{OpenRequested, ScriptOpenFailed, TabModel};
use crate::ui::time_fmt;

/// Resets a shared in-flight flag back to `false` on drop
struct ClearOnDrop<'a>(&'a Rc<Cell<bool>>);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl WorkspaceView {
    /// Wire the Open/Browse flow's two entities together: `tabs`'
    /// [`OpenRequested`] and `open_modal`'s own confirm/cancel/browse
    /// events.
    pub(super) fn subscribe_to_open_events(
        tabs: &Entity<TabModel>,
        open_modal: &Entity<OpenModalView>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(tabs, |this, _tabs, evt: &OpenRequested, cx| {
            this.handle_open_requested(*evt, cx);
        })
        .detach();
        cx.subscribe(tabs, |this, _tabs, evt: &ScriptOpenFailed, cx| {
            this.show_save_error(&format!("Failed to open {}", evt.file_name), cx);
        })
        .detach();
        cx.subscribe(open_modal, |this, _modal, evt: &OpenModalEvent, cx| {
            this.handle_open_modal_event(evt, cx);
        })
        .detach();
    }

    fn handle_open_requested(&mut self, evt: OpenRequested, cx: &mut Context<Self>) {
        match evt {
            OpenRequested::OpenPicker => self.open_open_modal(cx),
            OpenRequested::BrowseFiles => self.begin_browse_files(cx),
        }
    }

    /// Open the Open Script picker, seeded with the active connection's
    /// display name, its named scripts, the library listing, and which of
    /// each are already open on it (for dedupe).
    #[tracing::instrument(name = "workspace_open_open_modal", skip(self, cx))]
    fn open_open_modal(&mut self, cx: &mut Context<Self>) {
        let connection_name = self.connections.read(cx).active().map_or_else(
            || UNSAVED_CONNECTION_LABEL.to_owned(),
            |active| active.name.clone(),
        );

        let now = SystemTime::now();
        let sessions: Vec<SessionScript> = self
            .session_store
            .active_session_location()
            .and_then(|(root, key)| SessionDir::new(&root, key).list_scripts().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|entry| SessionScript {
                file_name: entry.file_name,
                relative_time: time_fmt::relative_time(now, entry.modified),
            })
            .collect();
        let open_session_tabs = self.tabs.read(cx).named_open_scripts_by_file();
        let open_library_tabs = self.tabs.read(cx).open_library_tabs();

        let library: Vec<LibraryScript> = self
            .library_dir
            .as_ref()
            .and_then(|dir| session_store::LibraryDir::at(dir).list().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|entry| LibraryScript {
                name: entry.name,
                relative_time: time_fmt::relative_time(now, entry.modified),
            })
            .collect();

        self.open_modal.update(cx, |modal, cx| {
            modal.open(
                connection_name,
                sessions,
                open_session_tabs,
                library,
                open_library_tabs,
                cx,
            );
        });
        cx.notify();
    }

    fn handle_open_modal_event(&mut self, evt: &OpenModalEvent, cx: &mut Context<Self>) {
        match evt {
            OpenModalEvent::Cancelled => {
                self.refocus_editor_on_next_render = true;
                cx.notify();
            }
            OpenModalEvent::BrowseFiles => self.begin_browse_files(cx),
            OpenModalEvent::Open(target) => self.apply_open_target(target.clone(), cx),
        }
    }

    /// Focus or open the tab `target` names
    fn apply_open_target(&mut self, target: PickerTarget, cx: &mut Context<Self>) {
        match target {
            PickerTarget::FocusTab(id) => {
                self.tabs.update(cx, |tabs, cx| tabs.set_active(id, cx));
            }
            PickerTarget::OpenLibrary(name) => {
                self.tabs
                    .update(cx, |tabs, cx| tabs.open_or_focus_library(&name, cx));
            }
            PickerTarget::OpenSessionScript(file_name) => {
                self.tabs.update(cx, |tabs, cx| {
                    tabs.open_or_focus_session_script(&file_name, cx);
                });
            }
        }
        cx.notify();
    }

    /// Go straight to the platform open-file dialog
    #[tracing::instrument(name = "workspace_begin_browse_files", skip(self, cx))]
    fn begin_browse_files(&mut self, cx: &mut Context<Self>) {
        if self.browse_dialog_in_flight.replace(true) {
            tracing::debug!("browse dialog already open; ignoring a second trigger");
            return;
        }
        let task = (self.open_files_prompt)(cx);
        let tabs = self.tabs.clone();
        let in_flight = self.browse_dialog_in_flight.clone();

        cx.spawn(async move |this, cx| {
            let _guard = ClearOnDrop(&in_flight);
            let Some(paths) = task.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read_result = cx
                .background_spawn({
                    let path = path.clone();
                    async move { session_store::external::load(&path) }
                })
                .await;
            let file_label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(crate::config::UNTITLED_SCRIPT_NAME)
                .to_owned();
            match read_result {
                Ok(Some(text)) => {
                    let _ = tabs.update(cx, |tabs, cx| {
                        tabs.open_or_focus_external(&path, file_label, &text, cx);
                    });
                }
                Ok(None) => {
                    tracing::warn!(path = %path.display(), "picked file no longer exists");
                    let _ = this.update(cx, |this, cx| {
                        this.show_save_error(&format!("{file_label} no longer exists"), cx);
                    });
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "failed to read picked file");
                    let _ = this.update(cx, |this, cx| {
                        this.show_save_error(&format!("Failed to open {file_label}"), cx);
                    });
                }
            }
        })
        .detach();
    }
}
