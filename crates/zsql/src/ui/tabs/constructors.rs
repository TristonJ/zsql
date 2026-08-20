//! `TabModel`'s tab-opening constructors: a live `Generated` preview, a
//! blank or pre-seeded `Script`, an external-file-backed `Script`, and a
//! read-only `Schema` tab

use gpui::{AppContext as _, Context};
use zsql_core::RelationKind;
use zsql_core::preview_state::PreviewQueryState;

use super::{Tab, TabId, TabKind, TabModel, canonicalize_or_self};
use crate::session_store::{ScriptBacking, ScriptFileName};
use crate::ui::schema_view::SchemaTabView;

impl TabModel {
    /// Open a `Generated` tab for `schema.relation` and make it active.
    /// Reuses the relation's existing live (never-edited) generated tab
    /// instead of creating a duplicate, if one exists
    pub fn open_or_reuse_generated(
        &mut self,
        schema: &str,
        relation: &str,
        cx: &mut Context<Self>,
    ) -> TabId {
        let key = (schema.to_owned(), relation.to_owned());
        if let Some(&id) = self.generated_by_relation.get(&key) {
            tracing::info!(tab_id = id, schema, relation, "reusing live generated tab");
            self.set_active(id, cx);
            return id;
        }

        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        // Read the exact text `preview_relation` (dispatched below) is about
        // to execute, so the buffer a user sees can never drift from what
        // actually runs.
        let sql = self.session.read(cx).preview_sql(schema, relation);
        editor.update(cx, |editor, cx| {
            editor.set_text(&sql, cx);
            editor.set_compact(true);
        });

        let page_size = self.session.read(cx).preview_limit();
        self.tabs.push(Tab {
            id,
            kind: TabKind::Generated {
                schema: schema.to_owned(),
                relation: relation.to_owned(),
                preview: PreviewQueryState::new(page_size),
            },
            title: relation.to_owned(),
            editor,
            dirty: false,
            last_run: None,
            schema_view: None,
        });
        self.generated_by_relation.insert(key, id);
        self.active = Some(id);

        let task = self.session.update(cx, |session, cx| {
            session.preview_relation(schema, relation, cx)
        });
        self.dispatch_run(id, format!("{schema}.{relation}"), task, false, cx);
        self.sync_preview_controls(cx);

        tracing::info!(tab_id = id, schema, relation, "opened generated tab");
        cx.notify();
        id
    }

    /// Open a new, empty `Script` tab titled `query-N.sql` and make it
    /// active
    pub fn new_script_tab(&mut self, cx: &mut Context<Self>) -> TabId {
        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        let title = super::script_title(self.next_script_number);
        self.next_script_number += 1;

        tracing::info!(tab_id = id, title = %title, "opened new script tab");
        // A freshly minted `query-N.sql` title is unique among every open
        // tab (`next_script_number` never repeats), so it is already its own
        // sibling file name with no disambiguation needed.
        let file = ScriptFileName::new(title.clone())
            .expect("a freshly minted query-N.sql title is always a valid ScriptFileName");
        self.tabs.push(Tab {
            id,
            kind: TabKind::Script {
                backing: ScriptBacking::SessionScratch { file },
            },
            title,
            editor,
            dirty: false,
            last_run: None,
            schema_view: None,
        });
        self.active = Some(id);
        self.sync_results_to_active(cx);
        self.sync_preview_controls(cx);
        cx.notify();
        id
    }

    /// Open a new session-owned `Script` tab titled `title` with `text`
    /// already in its buffer, and make it active.
    pub fn new_script_tab_with_content(
        &mut self,
        title: String,
        text: &str,
        cx: &mut Context<Self>,
    ) -> TabId {
        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        editor.update(cx, |editor, cx| editor.set_text(text, cx));

        tracing::info!(tab_id = id, title = %title, "opened script tab for a save-as copy");
        // `title` is already the tab's exact validated top-level file name
        // (a Save-as copy's chosen name, or a reopened script's own file
        // name), so it is its own sibling file with no disambiguation
        // needed.
        let file = ScriptFileName::new(title.clone())
            .expect("a validated top-level file name is always a valid ScriptFileName");
        self.tabs.push(Tab {
            id,
            kind: TabKind::Script {
                backing: ScriptBacking::SessionNamed { file },
            },
            title,
            editor,
            dirty: false,
            last_run: None,
            schema_view: None,
        });
        self.active = Some(id);
        self.sync_results_to_active(cx);
        self.sync_preview_controls(cx);
        cx.notify();
        id
    }

    /// Open a new `Script` tab backed by the external file at `path`, titled
    /// after its file name, with `text` (its on-disk content, or an empty
    /// string if it could not be read) already in its buffer, and make it
    /// active.
    pub fn new_external_tab(
        &mut self,
        path: std::path::PathBuf,
        title: String,
        text: &str,
        cx: &mut Context<Self>,
    ) -> TabId {
        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        editor.update(cx, |editor, cx| editor.set_text(text, cx));

        tracing::info!(tab_id = id, title = %title, path = %path.display(), "opened external file tab");
        self.tabs.push(Tab {
            id,
            kind: TabKind::Script {
                backing: ScriptBacking::External {
                    path,
                    saved_text: Some(text.to_owned()),
                },
            },
            title,
            editor,
            dirty: false,
            last_run: None,
            schema_view: None,
        });
        self.active = Some(id);
        self.sync_results_to_active(cx);
        self.sync_preview_controls(cx);
        cx.notify();
        id
    }

    /// Focus `path`'s already-open external-backed tab if one exists on this
    /// connection, else open a fresh one via [`Self::new_external_tab`].
    /// `title`/`text` are only used for the fresh-open case.
    pub fn open_or_focus_external(
        &mut self,
        path: &std::path::Path,
        title: String,
        text: &str,
        cx: &mut Context<Self>,
    ) -> TabId {
        let canonical = canonicalize_or_self(path);
        self.retire_carried_forward_external_entry(&canonical);
        if let Some(id) = self
            .tabs
            .iter()
            .find(|tab| {
                tab.external_path()
                    .is_some_and(|open| canonicalize_or_self(open) == canonical)
            })
            .map(Tab::id)
        {
            self.set_active(id, cx);
            return id;
        }
        self.new_external_tab(path.to_owned(), title, text, cx)
    }

    /// Open (or, if `schema.relation` already has one open, reuse/activate)
    /// a read-only `Schema` tab for `schema.relation` and make it active.
    /// `kind` is the relation's [`RelationKind`], shown in the tab's header
    /// kind pill.
    pub fn open_or_reuse_schema(
        &mut self,
        schema: &str,
        relation: &str,
        kind: RelationKind,
        cx: &mut Context<Self>,
    ) -> TabId {
        let key = (schema.to_owned(), relation.to_owned());
        if let Some(&id) = self.schema_by_relation.get(&key) {
            tracing::info!(tab_id = id, schema, relation, "reusing open schema tab");
            self.set_active(id, cx);
            return id;
        }

        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        let session = self.session.clone();
        let schema_view = cx.new(|cx| {
            SchemaTabView::new(&session, schema.to_owned(), relation.to_owned(), kind, cx)
        });

        self.tabs.push(Tab {
            id,
            kind: TabKind::Schema {
                schema: schema.to_owned(),
                relation: relation.to_owned(),
            },
            title: relation.to_owned(),
            editor,
            dirty: false,
            last_run: None,
            schema_view: Some(schema_view),
        });
        self.schema_by_relation.insert(key, id);
        self.active = Some(id);
        self.sync_results_to_active(cx);
        self.sync_preview_controls(cx);

        tracing::info!(tab_id = id, schema, relation, "opened schema tab");
        cx.notify();
        id
    }
}
