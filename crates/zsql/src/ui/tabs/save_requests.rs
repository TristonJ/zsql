//! Save/Save-as request routing for script tabs

use gpui::{App, Context, EventEmitter};

use super::{SCRIPT_TITLE_SUFFIX, Tab, TabId, TabKind, TabModel};
use crate::session_store::backing::SaveAction;
use crate::session_store::{LibraryName, ScriptBacking, ScriptFileName};

/// What pressing Save or Save-as on a tab asks the app to do
#[derive(Debug, Clone, Copy)]
pub enum SaveRequested {
    /// Open the Save (or Save-as) modal for `tab_id`
    OpenSaveModal { tab_id: TabId, as_copy: bool },
    /// Write `tab_id`'s library file directly with its current buffer text,
    /// no modal
    WriteLibraryDirect { tab_id: TabId },
    /// Write `tab_id`'s external file directly with its current buffer
    /// text, no modal
    WriteExternalDirect { tab_id: TabId },
}

impl EventEmitter<SaveRequested> for TabModel {}

/// A named session script could not be reopened from disk: the file was
/// moved or deleted, is not readable, or is not valid UTF-8
#[derive(Debug, Clone)]
pub struct ScriptOpenFailed {
    pub file_name: String,
}

impl EventEmitter<ScriptOpenFailed> for TabModel {}

impl TabModel {
    /// This tab's pure backing classification.
    /// `None` for a tab that does not exist or is not a `Script`.
    #[must_use]
    pub fn script_backing_of(&self, id: TabId) -> Option<ScriptBacking> {
        self.tab(id)?.script_backing().cloned()
    }

    /// This tab's current buffer text. `None` if the tab does not exist.
    #[must_use]
    pub fn tab_buffer_text(&self, id: TabId, cx: &App) -> Option<String> {
        Some(self.tab(id)?.editor.read(cx).text())
    }

    /// This tab's current title. `None` if the tab does not exist.
    #[must_use]
    pub fn tab_title_of(&self, id: TabId) -> Option<&str> {
        self.tab(id).map(Tab::title)
    }

    pub fn trigger_save(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.request_save(id, cx);
    }

    pub fn trigger_save_as(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.request_save_as(id, cx);
    }

    pub(super) fn request_save(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(backing) = self.script_backing_of(id) else {
            return;
        };
        match backing.save_action() {
            SaveAction::NoOp => {
                tracing::debug!(tab_id = id, "save: named session tab, already autosaved");
            }
            SaveAction::OpenModal => cx.emit(SaveRequested::OpenSaveModal {
                tab_id: id,
                as_copy: false,
            }),
            SaveAction::WriteLibrary => {
                cx.emit(SaveRequested::WriteLibraryDirect { tab_id: id });
            }
            SaveAction::WriteExternal => {
                cx.emit(SaveRequested::WriteExternalDirect { tab_id: id });
            }
        }
    }

    pub(super) fn request_save_as(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.script_backing_of(id).is_none() {
            return;
        }
        cx.emit(SaveRequested::OpenSaveModal {
            tab_id: id,
            as_copy: true,
        });
    }

    pub fn apply_renamed_title(&mut self, id: TabId, new_title: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id)
            && matches!(tab.kind, TabKind::Script { .. })
        {
            let file = ScriptFileName::new(new_title.clone())
                .expect("a confirmed rename target is always a valid ScriptFileName");
            tab.kind = TabKind::Script {
                backing: ScriptBacking::SessionNamed { file },
            };
            tab.title = new_title;
            cx.notify();
        }
    }

    /// Fold a completed "write this tab's buffer to the library and adopt
    /// that as its backing" operation back into `id`'s in-memory state.
    pub fn convert_to_library_backed(
        &mut self,
        id: TabId,
        library_name: String,
        saved_text: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id)
            && matches!(tab.kind, TabKind::Script { .. })
        {
            let name = LibraryName::new(library_name)
                .expect("a confirmed library save target is always a valid LibraryName");
            tab.title = format!("{}{SCRIPT_TITLE_SUFFIX}", name.as_str());
            tab.kind = TabKind::Script {
                backing: ScriptBacking::Library {
                    name,
                    saved_text: Some(saved_text),
                },
            };
            cx.notify();
        }
    }

    pub fn mark_backing_saved(&mut self, id: TabId, saved_text: String, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id)
            && let TabKind::Script {
                backing:
                    ScriptBacking::Library {
                        saved_text: slot, ..
                    }
                    | ScriptBacking::External {
                        saved_text: slot, ..
                    },
            } = &mut tab.kind
        {
            *slot = Some(saved_text);
            cx.notify();
        }
    }

    pub fn apply_library_rename(&mut self, id: TabId, new_name: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id)
            && let TabKind::Script {
                backing: ScriptBacking::Library { name, .. },
            } = &mut tab.kind
        {
            *name = LibraryName::new(new_name.to_owned())
                .expect("a confirmed library rename target is always a valid LibraryName");
            tab.title = format!("{new_name}{SCRIPT_TITLE_SUFFIX}");
            cx.notify();
        }
    }

    /// Every open, session-owned `Script` tab with a real name, paired with
    /// its on-disk sibling file name
    #[must_use]
    pub fn named_open_scripts_by_file(&self) -> Vec<(String, TabId)> {
        self.tabs
            .iter()
            .filter_map(|tab| match tab.script_backing() {
                Some(ScriptBacking::SessionNamed { file }) => {
                    Some((file.as_str().to_owned(), tab.id))
                }
                _ => None,
            })
            .collect()
    }

    /// Every open tab's library name paired with its `TabId`
    #[must_use]
    pub fn open_library_tabs(&self) -> Vec<(String, TabId)> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.library_name().map(|name| (name.to_owned(), tab.id)))
            .collect()
    }

    /// Focus `name`'s already-open library-backed tab if one exists on this
    /// connection, else open a fresh tab for it, loading its content from
    /// the library (falling back to an empty buffer if it cannot be read).
    ///
    /// `None` if `name` is not a valid [`LibraryName`]
    pub fn open_or_focus_library(&mut self, name: &str, cx: &mut Context<Self>) -> Option<TabId> {
        if let Some(id) = self
            .tabs
            .iter()
            .find(|tab| tab.library_name() == Some(name))
            .map(Tab::id)
        {
            self.set_active(id, cx);
            return Some(id);
        }

        let Ok(library_name) = LibraryName::new(name.to_owned()) else {
            tracing::warn!(name, "refusing to open an invalid library name");
            return None;
        };

        let library_text = self
            .library_dir
            .as_ref()
            .and_then(|dir| {
                crate::session_store::LibraryDir::at(dir)
                    .load(&library_name)
                    .ok()
            })
            .flatten();
        let buffer_text = library_text.unwrap_or_default();

        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        editor.update(cx, |editor, cx| editor.set_text(&buffer_text, cx));
        self.tabs.push(Tab {
            id,
            kind: TabKind::Script {
                backing: ScriptBacking::Library {
                    name: library_name,
                    saved_text: Some(buffer_text),
                },
            },
            title: format!("{name}{SCRIPT_TITLE_SUFFIX}"),
            editor,
            dirty: false,
            last_run: None,
            schema_view: None,
        });
        self.active = Some(id);
        self.sync_results_to_active(cx);
        self.sync_preview_controls(cx);
        tracing::info!(
            tab_id = id,
            library_name = name,
            "opened a library script tab"
        );
        cx.notify();
        Some(id)
    }

    /// Focus `file_name`'s already-open, named, session-owned tab if one
    /// exists on this connection, else open a fresh tab for it, loading its
    /// content straight from the active connection's session directory.
    ///
    /// Returns `None` and emits [`ScriptOpenFailed`] if `file_name` cannot
    /// be read (moved, deleted, permission-denied, or not valid UTF-8) or
    /// no session directory is resolved at all.
    pub fn open_or_focus_session_script(
        &mut self,
        file_name: &str,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        if let Some(id) = self
            .tabs
            .iter()
            .find(|tab| {
                matches!(
                    tab.script_backing(),
                    Some(ScriptBacking::SessionNamed { file }) if file.as_str() == file_name
                )
            })
            .map(Tab::id)
        {
            self.set_active(id, cx);
            return Some(id);
        }

        let Some(dir) = self.session_dir.clone() else {
            tracing::warn!(
                file_name,
                "no active session directory; refusing to reopen script"
            );
            cx.emit(ScriptOpenFailed {
                file_name: file_name.to_owned(),
            });
            return None;
        };
        let Ok(file) = ScriptFileName::new(file_name) else {
            tracing::warn!(file_name, "refusing to reopen an invalid script file name");
            cx.emit(ScriptOpenFailed {
                file_name: file_name.to_owned(),
            });
            return None;
        };
        let path = crate::session_store::SessionDir::at(&dir).named_path(&file);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(
                    file_name,
                    error = %err,
                    "failed to reopen named session script; refusing to open a tab that \
                     would overwrite it on the next autosave"
                );
                cx.emit(ScriptOpenFailed {
                    file_name: file_name.to_owned(),
                });
                return None;
            }
        };
        if let Some(claims) = &self.claims {
            crate::session_store::SessionDir::at(&dir)
                .reclaim_named_script_ref(file_name, file_name, claims);
        }
        let id = self.new_script_tab_with_content(file_name.to_owned(), &text, cx);
        tracing::info!(tab_id = id, file_name, "reopened a named session script");
        Some(id)
    }
}
