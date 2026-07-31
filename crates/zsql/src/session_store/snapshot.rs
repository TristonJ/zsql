//! The domain shape of a persisted tab session -- [`TabSessionSnapshot`],
//! its per-tab [`TabEntrySnapshot`], and the [`TabKind`] each entry carries
//! plus the conversions to and from the on-disk wire shapes

use zsql_core::preview_state::PreviewQueryState;

use super::SessionStoreError;
use super::backing::ScriptBacking;
use super::disk::{
    PersistedTab, ScriptRef, TabsFile, decode_external_ref, draft_file_name, encode_external_ref,
    is_external_ref,
};
use super::session_dir::LockedSessionDir;

/// What kind of buffer a tab holds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    /// A normal, freely-editable script buffer, identified by its
    /// [`ScriptBacking`].
    Script { backing: ScriptBacking },
    /// Auto-generated preview SQL for `schema.relation`, live for reuse
    /// until the buffer receives a manual edit
    Generated {
        schema: String,
        relation: String,
        preview: PreviewQueryState,
    },
    /// A read-only structural view of `schema.relation`'s columns, indexes,
    /// and constraints
    Schema { schema: String, relation: String },
}

/// One persisted tab: its display title, kind, and buffer text
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabEntrySnapshot {
    pub title: String,
    pub kind: TabKind,
    /// A session-owned script's full buffer, always present; a library- or
    /// external-backed script's session-scoped draft, present only while it
    /// diverges from its last saved content (`None` means the buffer
    /// matches that baseline exactly); `None` for a `Generated` entry
    pub buffer_text: Option<String>,
}

/// A connection's entire open-tab state
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabSessionSnapshot {
    /// Every open tab, in tab-bar order.
    pub tabs: Vec<TabEntrySnapshot>,
    /// Position in `tabs` of the tab that was active, if any.
    pub active_index: Option<usize>,
}

impl TabEntrySnapshot {
    /// Create a snapshot of one tab from its persisted shape, reading its
    /// sibling script or draft content through `dir`.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if a named file/draft ref is missing,
    /// unreadable, or escapes the session directory.
    fn from_persisted(
        dir: &LockedSessionDir<'_>,
        entry: PersistedTab,
    ) -> Result<Self, SessionStoreError> {
        match entry {
            PersistedTab::Script { title, file, draft } => {
                if is_external_ref(&file) {
                    let buffer_text = draft.as_deref().map(|f| dir.read_draft(f)).transpose()?;
                    return Ok(Self {
                        title,
                        kind: TabKind::Script {
                            backing: ScriptBacking::External {
                                path: decode_external_ref(&file),
                                saved_text: None,
                            },
                        },
                        buffer_text,
                    });
                }
                let Some(script_ref) = ScriptRef::parse(&file) else {
                    return Err(SessionStoreError::UnsafeRef(file));
                };
                match script_ref {
                    ScriptRef::Library(name) => {
                        let buffer_text =
                            draft.as_deref().map(|f| dir.read_draft(f)).transpose()?;
                        Ok(Self {
                            title,
                            kind: TabKind::Script {
                                backing: ScriptBacking::Library {
                                    name,
                                    saved_text: None,
                                },
                            },
                            buffer_text,
                        })
                    }
                    ScriptRef::Scratch(script_file) => {
                        let buffer_text = dir.read_scratch(&script_file)?;
                        Ok(Self {
                            title,
                            kind: TabKind::Script {
                                backing: ScriptBacking::SessionScratch { file: script_file },
                            },
                            buffer_text: Some(buffer_text),
                        })
                    }
                    ScriptRef::Session(script_file) => {
                        let buffer_text = dir.read_named(&script_file)?;
                        Ok(Self {
                            title,
                            kind: TabKind::Script {
                                backing: ScriptBacking::SessionNamed { file: script_file },
                            },
                            buffer_text: Some(buffer_text),
                        })
                    }
                }
            }
            PersistedTab::Generated {
                title,
                schema,
                relation,
                preview_state,
            } => Ok(Self {
                title,
                kind: TabKind::Generated {
                    schema,
                    relation,
                    preview: preview_state,
                },
                buffer_text: None,
            }),
        }
    }

    /// Build this entry's persisted shape, writing every file it owns (its
    /// sibling script or draft) via `dir` as a side effect.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if a write fails.
    pub(crate) fn persist(
        &self,
        dir: &LockedSessionDir<'_>,
    ) -> Result<PersistedTab, SessionStoreError> {
        match &self.kind {
            TabKind::Script { backing } => {
                let buffer_text = self.buffer_text.as_deref().unwrap_or_default();
                match backing {
                    ScriptBacking::SessionScratch { file } => {
                        dir.write_scratch(file, buffer_text)?;
                        Ok(PersistedTab::Script {
                            title: self.title.clone(),
                            file: ScriptRef::Scratch(file.clone()).to_ref_string(),
                            draft: None,
                        })
                    }
                    ScriptBacking::SessionNamed { file } => {
                        dir.write_named(file, buffer_text)?;
                        Ok(PersistedTab::Script {
                            title: self.title.clone(),
                            file: ScriptRef::Session(file.clone()).to_ref_string(),
                            draft: None,
                        })
                    }
                    ScriptBacking::Library { .. } | ScriptBacking::External { .. } => {
                        let draft = self
                            .buffer_text
                            .as_ref()
                            .map(|text| {
                                let draft_file = draft_file_name(backing).expect(
                                    "library/external backing always has a draft file name",
                                );
                                dir.write_draft(&draft_file, text)?;
                                Ok::<_, SessionStoreError>(draft_file)
                            })
                            .transpose()?;
                        let file = match backing {
                            ScriptBacking::Library { name, .. } => {
                                ScriptRef::Library(name.clone()).to_ref_string()
                            }
                            ScriptBacking::External { path, .. } => encode_external_ref(path),
                            _ => unreachable!("outer arm matched library/external"),
                        };
                        Ok(PersistedTab::Script {
                            title: self.title.clone(),
                            file,
                            draft,
                        })
                    }
                }
            }
            TabKind::Generated {
                schema,
                relation,
                preview,
            } => Ok(PersistedTab::Generated {
                title: self.title.clone(),
                schema: schema.clone(),
                relation: relation.clone(),
                preview_state: preview.clone(),
            }),
            TabKind::Schema { .. } => {
                unreachable!("Schema tabs are filtered out before a snapshot is built")
            }
        }
    }
}

impl TabSessionSnapshot {
    /// Load a snapshot of a connection's open tabs from a persisted `tabs.toml`
    /// file, reading every tab's sibling script or draft content through `dir`.
    /// An entry whose file is missing, unreadable, or resolves outside the
    /// session directory is logged and skipped rather than failing the whole
    /// snapshot, and `active_index` is remapped over the surviving entries so
    /// it still points at the same tab.
    pub(in crate::session_store) fn from_file(file: TabsFile, dir: &LockedSessionDir<'_>) -> Self {
        let mut tabs = Vec::with_capacity(file.tabs.len());
        let mut surviving_original_indices = Vec::with_capacity(file.tabs.len());
        for (original_index, entry) in file.tabs.into_iter().enumerate() {
            match TabEntrySnapshot::from_persisted(dir, entry) {
                Ok(snapshot) => {
                    tabs.push(snapshot);
                    surviving_original_indices.push(original_index);
                }
                Err(err) => {
                    tracing::warn!(
                        original_index,
                        error = %err,
                        "skipping unreadable session tab entry; the rest of the session still loads"
                    );
                }
            }
        }

        let active_index = file.active.and_then(|active| {
            surviving_original_indices
                .iter()
                .position(|&original| original == active)
        });
        let snapshot = Self { tabs, active_index };
        tracing::info!(tab_count = snapshot.tabs.len(), "tab session load");
        snapshot
    }
}
