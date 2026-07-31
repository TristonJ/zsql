//! Rebuilding a [`TabModel`]'s tabs from a loaded [`TabSessionSnapshot`]

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::{Context, Entity};
use zsql_core::preview_state::PreviewQueryState;
use zsql_editor::EditorView;

use super::{Tab, TabId, TabKind, TabModel, canonicalize_or_self};
use crate::session_store;
use crate::session_store::{ScriptBacking, TabEntrySnapshot, TabSessionSnapshot};

impl TabModel {
    /// Rebuild `self.tabs` from `snapshot`'s entries
    pub(super) fn restore_tabs(&mut self, snapshot: &TabSessionSnapshot, cx: &mut Context<Self>) {
        self.next_script_number = snapshot
            .tabs
            .iter()
            .filter_map(|entry| super::parse_script_number(&entry.title))
            .max()
            .map_or(1, |highest| highest + 1);

        // Maps each snapshot index to the `TabId` it restored to, or `None`
        // for an entry this pass skipped
        let mut restored_ids: Vec<Option<TabId>> = Vec::with_capacity(snapshot.tabs.len());
        // Canonicalized paths of every External entry successfully restored
        // so far this pass
        let mut restored_external_paths: HashSet<PathBuf> = HashSet::new();

        for entry in &snapshot.tabs {
            if let TabKind::Script {
                backing: ScriptBacking::External { path, .. },
            } = &entry.kind
                && restored_external_paths.contains(&canonicalize_or_self(path))
            {
                tracing::warn!(
                    path = %path.display(),
                    "skipping a duplicate restored External entry for an already-restored path"
                );
                restored_ids.push(None);
                continue;
            }

            let id = self.allocate_id();
            let editor = Self::build_editor(id, cx);
            let restored = self.restore_entry_kind(id, entry, &editor, cx);

            let Some(kind) = restored else {
                self.carried_forward_entries.push(entry.clone());
                restored_ids.push(None);
                continue;
            };

            if let TabKind::Script {
                backing: ScriptBacking::External { path, .. },
            } = &entry.kind
            {
                restored_external_paths.insert(canonicalize_or_self(path));
            }

            self.tabs.push(Tab {
                id,
                kind,
                title: entry.title.clone(),
                editor,
                dirty: false,
                last_run: None,
                schema_view: None,
            });
            restored_ids.push(Some(id));
        }

        self.active = snapshot
            .active_index
            .and_then(|index| restored_ids.get(index).copied().flatten())
            .or_else(|| self.tabs.first().map(Tab::id));
    }

    /// Restore one entry's kind setting `editor`'s text as a side effect.
    /// `None` only for an `External` entry [`Self::restore_external_tab`]
    /// could not open
    fn restore_entry_kind(
        &mut self,
        id: TabId,
        entry: &TabEntrySnapshot,
        editor: &Entity<EditorView>,
        cx: &mut Context<Self>,
    ) -> Option<TabKind> {
        match &entry.kind {
            TabKind::Script {
                backing: ScriptBacking::SessionScratch { .. } | ScriptBacking::SessionNamed { .. },
            } => {
                let buffer_text = entry.buffer_text.as_deref().unwrap_or_default();
                editor.update(cx, |editor, cx| editor.set_text(buffer_text, cx));
                Some(entry.kind.clone())
            }
            TabKind::Script {
                backing: ScriptBacking::Library { name, .. },
            } => {
                let backing =
                    self.restore_library_tab(id, name, entry.buffer_text.as_deref(), editor, cx);
                Some(TabKind::Script { backing })
            }
            TabKind::Script {
                backing: ScriptBacking::External { path, .. },
            } => Self::restore_external_tab(id, path, entry.buffer_text.as_deref(), editor, cx)
                .map(|backing| TabKind::Script { backing }),
            TabKind::Generated {
                schema,
                relation,
                preview,
            } => Some(self.restore_generated_tab(id, schema, relation, preview, editor, cx)),
            TabKind::Schema { .. } => {
                unreachable!("a Schema tab is never persisted, so never appears in a snapshot")
            }
        }
    }

    /// Restore one external-backed entry's editor buffer
    fn restore_external_tab(
        id: TabId,
        path: &std::path::Path,
        draft_text: Option<&str>,
        editor: &Entity<EditorView>,
        cx: &mut Context<Self>,
    ) -> Option<ScriptBacking> {
        if let Some(draft) = draft_text {
            editor.update(cx, |editor, cx| editor.set_text(draft, cx));
            let saved_text = match session_store::external::load(path) {
                Ok(Some(text)) => Some(text),
                // The file is simply gone: there is nothing to diverge
                // from
                Ok(None) => Some(draft.to_owned()),
                Err(err) => {
                    tracing::warn!(
                        tab_id = id,
                        path = %path.display(),
                        error = %err,
                        "external file unreadable on restore; keeping the tab diverged rather \
                         than trusting an unconfirmed draft to already match it"
                    );
                    None
                }
            };
            tracing::info!(
                tab_id = id,
                path = %path.display(),
                diverged = true,
                "restored an external-backed tab from its draft"
            );
            return Some(ScriptBacking::External {
                path: path.to_owned(),
                saved_text,
            });
        }

        match session_store::external::load(path) {
            Ok(Some(text)) => {
                editor.update(cx, |editor, cx| editor.set_text(&text, cx));
                tracing::info!(tab_id = id, path = %path.display(), "restored an external-backed tab");
                Some(ScriptBacking::External {
                    path: path.to_owned(),
                    saved_text: Some(text),
                })
            }
            Ok(None) => {
                tracing::warn!(
                    tab_id = id,
                    path = %path.display(),
                    "external file missing on restore with no draft to fall back to; skipping tab"
                );
                None
            }
            Err(err) => {
                tracing::warn!(
                    tab_id = id,
                    path = %path.display(),
                    error = %err,
                    "external file unreadable on restore with no draft to fall back to; skipping tab"
                );
                None
            }
        }
    }

    /// Restore one library-backed entry's editor buffer
    fn restore_library_tab(
        &self,
        id: TabId,
        name: &session_store::LibraryName,
        draft_text: Option<&str>,
        editor: &Entity<EditorView>,
        cx: &mut Context<Self>,
    ) -> ScriptBacking {
        let library_name = name.as_str();
        let library_read = self
            .library_dir
            .as_ref()
            .map(|dir| session_store::LibraryDir::at(dir).load(name));
        let library_text = match &library_read {
            Some(Ok(text)) => text.clone(),
            _ => None,
        };
        let buffer_text = draft_text
            .map(str::to_owned)
            .or_else(|| library_text.clone())
            .unwrap_or_default();
        editor.update(cx, |editor, cx| editor.set_text(&buffer_text, cx));

        let saved_text = match &library_read {
            Some(Ok(Some(text))) => Some(text.clone()),
            Some(Ok(None)) | None => Some(buffer_text.clone()),
            Some(Err(err)) => {
                tracing::warn!(
                    tab_id = id,
                    library_name,
                    error = %err,
                    "library file unreadable on restore; keeping the tab diverged rather \
                     than trusting an unconfirmed draft to already match it"
                );
                None
            }
        };
        tracing::info!(
            tab_id = id,
            library_name,
            diverged = draft_text.is_some(),
            "restored a library-backed tab"
        );
        ScriptBacking::Library {
            name: name.clone(),
            saved_text,
        }
    }

    /// Regenerate one generated entry's editor buffer from its persisted
    /// `preview_state`
    fn restore_generated_tab(
        &mut self,
        id: TabId,
        schema: &str,
        relation: &str,
        preview_state: &PreviewQueryState,
        editor: &Entity<EditorView>,
        cx: &mut Context<Self>,
    ) -> TabKind {
        let filters = preview_state.filters();
        let _span = tracing::info_span!(
            "tab_session_restore_generated",
            tab_id = id,
            schema,
            relation,
            limit = preview_state.page_size(),
            offset = preview_state.offset(),
            filtered = !filters.is_empty()
        )
        .entered();
        let sql = self.session.read(cx).preview_sql_windowed(
            schema,
            relation,
            preview_state.sort_pair(),
            preview_state.page_size(),
            preview_state.offset(),
            filters,
        );
        tracing::info!("regenerated a restored generated tab's buffer");
        editor.update(cx, |editor, cx| {
            editor.set_text(&sql, cx);
            editor.set_compact(true);
        });
        self.generated_by_relation
            .insert((schema.to_owned(), relation.to_owned()), id);
        let mut preview_state = preview_state.clone();
        preview_state.clear_resolved_totals();
        TabKind::Generated {
            schema: schema.to_owned(),
            relation: relation.to_owned(),
            preview: preview_state,
        }
    }
}
