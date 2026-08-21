//! The `[keybindings]` TOML section: one sub-table per registration site,
//! every field `Option<Keystrokes>` so an unset field means "use the
//! built-in default" and never appears when the config is saved.

use serde::{Deserialize, Serialize};

use super::Keystrokes;

/// True when `value` equals its type's default -- used to skip serializing
/// an all-default keybindings sub-table entirely, rather than writing an
/// empty `[section]` header for it.
pub(crate) fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// The `[keybindings]` config section: one sub-table per key-binding
/// registration site. An unset field falls back to that action's built-in
/// default; see [`super::resolve`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    #[serde(skip_serializing_if = "is_default")]
    pub editor: EditorKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub text_field: TextFieldKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub results: ResultsKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub quick_find: QuickFindKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub editor_find: EditorFindKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub cell_edit: CellEditKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub value_panel: ValuePanelKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub sidebar: SidebarKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub schema_view: SchemaViewKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub open_modal: OpenModalKeybindings,
    #[serde(skip_serializing_if = "is_default")]
    pub save_modal: SaveModalKeybindings,
}

/// `[keybindings.editor]`: the SQL editor pane's move/select/edit/run/save
/// bindings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_left: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_right: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_up: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_down: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_line_start: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_line_end: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_document_start: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_document_end: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_left: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_right: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_up: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_down: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_line_start: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_line_end: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_document_start: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_document_end: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_all: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backspace: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete_forward: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newline: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paste: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_query: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_script: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_script_as: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_script: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browse_script_files: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redo: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_find: Option<Keystrokes>,
}

/// `[keybindings.text_field]`: the shared single-line `TextField`'s
/// move/select/edit bindings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextFieldKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_left: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_right: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_home: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_end: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_left: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_right: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_home: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_end: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_all: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backspace: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete_forward: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paste: Option<Keystrokes>,
}

/// `[keybindings.results]`: the results grid's own bindings. The apply
/// chord's canonical home; see [`super::resolve`] for the legacy
/// `staging.apply_keybinding` fallback order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResultsKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_up: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_down: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_left: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_right: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toggle_value_panel: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_value_panel: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_value_panel: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_page: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_quick_find: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_staged: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_cell: Option<Keystrokes>,
}

/// `[keybindings.quick_find]`: the results grid's inline quick-find row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickFindKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<Keystrokes>,
}

/// `[keybindings.editor_find]`: the SQL editor pane's inline find bar.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorFindKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<Keystrokes>,
}

/// `[keybindings.cell_edit]`: the results grid's cell-edit popover.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellEditKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel: Option<Keystrokes>,
}

/// `[keybindings.value_panel]`: the results grid's cell value panel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ValuePanelKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_up: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_down: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_collapse: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_expand: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_tree_node_value: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_tree_node_path: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_panel_from_panel: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_grid_from_panel: Option<Keystrokes>,
}

/// `[keybindings.sidebar]`: the schema sidebar's own bindings, including its
/// find row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_find: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_find: Option<Keystrokes>,
}

/// `[keybindings.schema_view]`: the read-only schema tab.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchemaViewKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<Keystrokes>,
}

/// `[keybindings.open_modal]`: the Open Script picker's row navigation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenModalKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_previous_row: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_next_row: Option<Keystrokes>,
}

/// `[keybindings.save_modal]`: the Save Script modal's destination
/// navigation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveModalKeybindings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_previous_destination: Option<Keystrokes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_next_destination: Option<Keystrokes>,
}

#[cfg(test)]
mod tests {
    use super::KeybindingsConfig;

    #[test]
    fn a_default_keybindings_config_serializes_to_an_empty_toml_document() {
        let text = toml::to_string(&KeybindingsConfig::default()).unwrap();
        assert_eq!(text, "");
    }

    #[test]
    fn a_partial_editor_only_override_leaves_every_other_field_unset() {
        let parsed: KeybindingsConfig = toml::from_str("[editor]\nrun_query = \"f5\"\n").unwrap();
        assert_eq!(parsed.editor.run_query.unwrap().0, vec!["f5".to_owned()]);
        assert!(parsed.editor.save_script.is_none());
        assert!(parsed.editor.open_find.is_none());
        assert!(parsed.results.copy.is_none());
        assert!(parsed.sidebar.open_find.is_none());
        assert!(parsed.editor_find.next.is_none());
    }

    #[test]
    fn a_partial_editor_find_only_override_leaves_every_other_field_unset() {
        let parsed: KeybindingsConfig = toml::from_str("[editor_find]\nnext = \"f7\"\n").unwrap();
        assert_eq!(parsed.editor_find.next.unwrap().0, vec!["f7".to_owned()]);
        assert!(parsed.editor_find.prev.is_none());
        assert!(parsed.editor_find.close.is_none());
        assert!(parsed.editor.open_find.is_none());
    }
}
