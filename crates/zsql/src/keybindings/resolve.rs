//! Pure resolution: turns a [`super::KeybindingsConfig`] (plus the legacy
//! `staging.apply_keybinding`) into the keystroke lists every registration
//! site consumes. No `gpui` window or `App` is needed, so this is testable
//! without opening a window.

use zsql_editor::EditorBindings;
use zsql_ui::text_field::TextFieldBindings;

use super::Keystrokes;
use super::config::{
    CellEditKeybindings, EditorFindKeybindings, EditorKeybindings, OpenModalKeybindings,
    QuickFindKeybindings, ResultsKeybindings, SaveModalKeybindings, SchemaViewKeybindings,
    SidebarKeybindings, TextFieldKeybindings, ValuePanelKeybindings,
};
use crate::config::{Config, StagingConfig};
use crate::ui::open_modal::OpenModalBindings;
use crate::ui::results::ResultsBindings;
use crate::ui::save_modal::SaveModalBindings;
use crate::ui::schema_view::SchemaViewBindings;
use crate::ui::sidebar::SidebarBindings;
use crate::ui::value_panel::ValuePanelBindings;

/// Resolve one action field: `resolve!(cfg, default, "area", field)` expands
/// to a `resolve_field` call naming `"keybindings.area.field"` as the config
/// key, keeping every call site's config-key string mechanically in sync
/// with the field it names.
macro_rules! resolve {
    ($cfg:expr, $default:expr, $area:literal, $field:ident) => {
        resolve_field(
            $cfg.$field.as_ref(),
            concat!("keybindings.", $area, ".", stringify!($field)),
            &$default.$field,
        )
    };
}

/// Every registration site's resolved keystrokes, built once at startup
/// from [`Config`] by [`resolve`].
pub struct ResolvedKeybindings {
    pub editor: EditorBindings,
    pub text_field: TextFieldBindings,
    pub results: ResultsBindings,
    pub value_panel: ValuePanelBindings,
    pub sidebar: SidebarBindings,
    pub schema_view: SchemaViewBindings,
    pub open_modal: OpenModalBindings,
    pub save_modal: SaveModalBindings,
}

/// Resolve `config`'s `[keybindings]` section (plus the legacy
/// `staging.apply_keybinding`) into every registration site's keystrokes.
/// Call once at startup; the result is never recomputed afterward.
#[tracing::instrument(name = "resolve_keybindings", skip(config))]
pub fn resolve(config: &Config) -> ResolvedKeybindings {
    ResolvedKeybindings {
        editor: resolve_editor(&config.keybindings.editor, &config.keybindings.editor_find),
        text_field: resolve_text_field(&config.keybindings.text_field),
        results: resolve_results(
            &config.keybindings.results,
            &config.keybindings.quick_find,
            &config.keybindings.cell_edit,
            &config.staging,
        ),
        value_panel: resolve_value_panel(&config.keybindings.value_panel),
        sidebar: resolve_sidebar(&config.keybindings.sidebar),
        schema_view: resolve_schema_view(&config.keybindings.schema_view),
        open_modal: resolve_open_modal(&config.keybindings.open_modal),
        save_modal: resolve_save_modal(&config.keybindings.save_modal),
    }
}

/// The keystrokes in `keystrokes` that [`gpui::Keystroke::parse`] accepts.
/// Each rejected entry logs a `tracing::warn!` naming `config_key` and the
/// offending string, and is dropped -- the rest of the list survives.
fn valid_keystrokes(keystrokes: &Keystrokes, config_key: &str) -> Vec<String> {
    keystrokes
        .0
        .iter()
        .filter(|keystroke| match gpui::Keystroke::parse(keystroke) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    config_key,
                    keystroke = keystroke.as_str(),
                    %error,
                    "invalid keystroke in config, ignoring"
                );
                false
            }
        })
        .cloned()
        .collect()
}

/// Resolve one action's configured keystrokes against its `default`:
/// `configured` wins if present and at least one of its entries parses,
/// otherwise `default` is used. An empty `configured` list (after dropping
/// invalid entries) also falls back to `default`.
fn resolve_field(
    configured: Option<&Keystrokes>,
    config_key: &str,
    default: &[String],
) -> Vec<String> {
    let Some(keystrokes) = configured else {
        return default.to_vec();
    };
    let valid = valid_keystrokes(keystrokes, config_key);
    if valid.is_empty() {
        tracing::warn!(
            config_key,
            "no valid keystrokes configured, falling back to the default"
        );
        default.to_vec()
    } else {
        valid
    }
}

/// Resolve the apply-staged action: `keybindings.results.apply_staged` wins
/// when set and valid; otherwise the legacy `staging.apply_keybinding`
/// (validated the same way) is used; otherwise the built-in default.
fn resolve_apply_staged(
    configured: Option<&Keystrokes>,
    legacy: &str,
    default: &[String],
) -> Vec<String> {
    if let Some(keystrokes) = configured {
        let valid = valid_keystrokes(keystrokes, "keybindings.results.apply_staged");
        if !valid.is_empty() {
            return valid;
        }
    }
    let legacy_keystrokes = Keystrokes(vec![legacy.to_owned()]);
    let valid_legacy = valid_keystrokes(&legacy_keystrokes, "staging.apply_keybinding");
    if !valid_legacy.is_empty() {
        if legacy != StagingConfig::default().apply_keybinding {
            tracing::info!(
                keystrokes = ?valid_legacy,
                "resolved apply-staged from the legacy staging.apply_keybinding setting"
            );
        }
        return valid_legacy;
    }
    default.to_vec()
}

fn resolve_editor(cfg: &EditorKeybindings, find_cfg: &EditorFindKeybindings) -> EditorBindings {
    let d = EditorBindings::default();
    EditorBindings {
        move_left: resolve!(cfg, d, "editor", move_left),
        move_right: resolve!(cfg, d, "editor", move_right),
        move_up: resolve!(cfg, d, "editor", move_up),
        move_down: resolve!(cfg, d, "editor", move_down),
        move_line_start: resolve!(cfg, d, "editor", move_line_start),
        move_line_end: resolve!(cfg, d, "editor", move_line_end),
        move_document_start: resolve!(cfg, d, "editor", move_document_start),
        move_document_end: resolve!(cfg, d, "editor", move_document_end),
        select_left: resolve!(cfg, d, "editor", select_left),
        select_right: resolve!(cfg, d, "editor", select_right),
        select_up: resolve!(cfg, d, "editor", select_up),
        select_down: resolve!(cfg, d, "editor", select_down),
        select_line_start: resolve!(cfg, d, "editor", select_line_start),
        select_line_end: resolve!(cfg, d, "editor", select_line_end),
        select_document_start: resolve!(cfg, d, "editor", select_document_start),
        select_document_end: resolve!(cfg, d, "editor", select_document_end),
        select_all: resolve!(cfg, d, "editor", select_all),
        backspace: resolve!(cfg, d, "editor", backspace),
        delete_forward: resolve!(cfg, d, "editor", delete_forward),
        newline: resolve!(cfg, d, "editor", newline),
        copy: resolve!(cfg, d, "editor", copy),
        cut: resolve!(cfg, d, "editor", cut),
        paste: resolve!(cfg, d, "editor", paste),
        run_query: resolve!(cfg, d, "editor", run_query),
        save_script: resolve!(cfg, d, "editor", save_script),
        save_script_as: resolve!(cfg, d, "editor", save_script_as),
        open_script: resolve!(cfg, d, "editor", open_script),
        browse_script_files: resolve!(cfg, d, "editor", browse_script_files),
        undo: resolve!(cfg, d, "editor", undo),
        redo: resolve!(cfg, d, "editor", redo),
        open_find: resolve!(cfg, d, "editor", open_find),
        find_next: resolve_field(
            find_cfg.next.as_ref(),
            "keybindings.editor_find.next",
            &d.find_next,
        ),
        find_prev: resolve_field(
            find_cfg.prev.as_ref(),
            "keybindings.editor_find.prev",
            &d.find_prev,
        ),
        close_find: resolve_field(
            find_cfg.close.as_ref(),
            "keybindings.editor_find.close",
            &d.close_find,
        ),
    }
}

fn resolve_text_field(cfg: &TextFieldKeybindings) -> TextFieldBindings {
    let d = TextFieldBindings::default();
    TextFieldBindings {
        move_left: resolve!(cfg, d, "text_field", move_left),
        move_right: resolve!(cfg, d, "text_field", move_right),
        move_home: resolve!(cfg, d, "text_field", move_home),
        move_end: resolve!(cfg, d, "text_field", move_end),
        select_left: resolve!(cfg, d, "text_field", select_left),
        select_right: resolve!(cfg, d, "text_field", select_right),
        select_home: resolve!(cfg, d, "text_field", select_home),
        select_end: resolve!(cfg, d, "text_field", select_end),
        select_all: resolve!(cfg, d, "text_field", select_all),
        backspace: resolve!(cfg, d, "text_field", backspace),
        delete_forward: resolve!(cfg, d, "text_field", delete_forward),
        submit: resolve!(cfg, d, "text_field", submit),
        copy: resolve!(cfg, d, "text_field", copy),
        cut: resolve!(cfg, d, "text_field", cut),
        paste: resolve!(cfg, d, "text_field", paste),
    }
}

fn resolve_results(
    results_cfg: &ResultsKeybindings,
    quick_find_cfg: &QuickFindKeybindings,
    cell_edit_cfg: &CellEditKeybindings,
    staging_cfg: &StagingConfig,
) -> ResultsBindings {
    let d = ResultsBindings::default();
    ResultsBindings {
        copy: resolve!(results_cfg, d, "results", copy),
        cell_up: resolve!(results_cfg, d, "results", cell_up),
        cell_down: resolve!(results_cfg, d, "results", cell_down),
        cell_left: resolve!(results_cfg, d, "results", cell_left),
        cell_right: resolve!(results_cfg, d, "results", cell_right),
        toggle_value_panel: resolve!(results_cfg, d, "results", toggle_value_panel),
        close_value_panel: resolve!(results_cfg, d, "results", close_value_panel),
        focus_value_panel: resolve!(results_cfg, d, "results", focus_value_panel),
        prev_page: resolve!(results_cfg, d, "results", prev_page),
        next_page: resolve!(results_cfg, d, "results", next_page),
        open_quick_find: resolve!(results_cfg, d, "results", open_quick_find),
        apply_staged: resolve_apply_staged(
            results_cfg.apply_staged.as_ref(),
            &staging_cfg.apply_keybinding,
            &d.apply_staged,
        ),
        edit_cell: resolve!(results_cfg, d, "results", edit_cell),
        quick_find_prev: resolve_field(
            quick_find_cfg.prev.as_ref(),
            "keybindings.quick_find.prev",
            &d.quick_find_prev,
        ),
        quick_find_next: resolve_field(
            quick_find_cfg.next.as_ref(),
            "keybindings.quick_find.next",
            &d.quick_find_next,
        ),
        quick_find_close: resolve_field(
            quick_find_cfg.close.as_ref(),
            "keybindings.quick_find.close",
            &d.quick_find_close,
        ),
        cancel_cell_edit: resolve_field(
            cell_edit_cfg.cancel.as_ref(),
            "keybindings.cell_edit.cancel",
            &d.cancel_cell_edit,
        ),
    }
}

fn resolve_value_panel(cfg: &ValuePanelKeybindings) -> ValuePanelBindings {
    let d = ValuePanelBindings::default();
    ValuePanelBindings {
        tree_up: resolve!(cfg, d, "value_panel", tree_up),
        tree_down: resolve!(cfg, d, "value_panel", tree_down),
        tree_collapse: resolve!(cfg, d, "value_panel", tree_collapse),
        tree_expand: resolve!(cfg, d, "value_panel", tree_expand),
        copy_tree_node_value: resolve!(cfg, d, "value_panel", copy_tree_node_value),
        copy_tree_node_path: resolve!(cfg, d, "value_panel", copy_tree_node_path),
        close_panel_from_panel: resolve!(cfg, d, "value_panel", close_panel_from_panel),
        focus_grid_from_panel: resolve!(cfg, d, "value_panel", focus_grid_from_panel),
    }
}

fn resolve_sidebar(cfg: &SidebarKeybindings) -> SidebarBindings {
    let d = SidebarBindings::default();
    SidebarBindings {
        open_find: resolve!(cfg, d, "sidebar", open_find),
        close_find: resolve!(cfg, d, "sidebar", close_find),
    }
}

fn resolve_schema_view(cfg: &SchemaViewKeybindings) -> SchemaViewBindings {
    let d = SchemaViewBindings::default();
    SchemaViewBindings {
        copy: resolve!(cfg, d, "schema_view", copy),
    }
}

fn resolve_open_modal(cfg: &OpenModalKeybindings) -> OpenModalBindings {
    let d = OpenModalBindings::default();
    OpenModalBindings {
        select_previous_row: resolve!(cfg, d, "open_modal", select_previous_row),
        select_next_row: resolve!(cfg, d, "open_modal", select_next_row),
    }
}

fn resolve_save_modal(cfg: &SaveModalKeybindings) -> SaveModalBindings {
    let d = SaveModalBindings::default();
    SaveModalBindings {
        select_previous_destination: resolve!(cfg, d, "save_modal", select_previous_destination),
        select_next_destination: resolve!(cfg, d, "save_modal", select_next_destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::config::KeybindingsConfig;

    fn keystrokes(entries: &[&str]) -> Keystrokes {
        Keystrokes(entries.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn a_configured_valid_keystroke_wins_over_the_default() {
        let cfg = EditorKeybindings {
            run_query: Some(keystrokes(&["f5"])),
            ..Default::default()
        };
        let resolved = resolve_editor(&cfg, &EditorFindKeybindings::default());
        assert_eq!(resolved.run_query, vec!["f5".to_owned()]);
    }

    #[test]
    fn an_unset_field_falls_back_to_the_default() {
        let cfg = EditorKeybindings::default();
        let resolved = resolve_editor(&cfg, &EditorFindKeybindings::default());
        assert_eq!(resolved.run_query, EditorBindings::default().run_query);
    }

    #[test]
    fn an_invalid_keystroke_falls_back_to_the_default_and_does_not_panic() {
        let cfg = EditorKeybindings {
            run_query: Some(keystrokes(&["not-a-key"])),
            ..Default::default()
        };
        let resolved = resolve_editor(&cfg, &EditorFindKeybindings::default());
        assert_eq!(resolved.run_query, EditorBindings::default().run_query);
    }

    #[test]
    fn a_mixed_list_keeps_the_valid_entries_and_drops_only_the_invalid_one() {
        let cfg = EditorKeybindings {
            run_query: Some(keystrokes(&["f5", "not-a-key", "f6"])),
            ..Default::default()
        };
        let resolved = resolve_editor(&cfg, &EditorFindKeybindings::default());
        assert_eq!(resolved.run_query, vec!["f5".to_owned(), "f6".to_owned()]);
    }

    #[test]
    fn an_all_invalid_list_falls_back_to_the_default() {
        let cfg = EditorKeybindings {
            run_query: Some(keystrokes(&["not-a-key", "also-not-a-key"])),
            ..Default::default()
        };
        let resolved = resolve_editor(&cfg, &EditorFindKeybindings::default());
        assert_eq!(resolved.run_query, EditorBindings::default().run_query);
    }

    #[test]
    fn an_empty_configured_list_falls_back_to_the_default() {
        let cfg = EditorKeybindings {
            run_query: Some(Keystrokes(Vec::new())),
            ..Default::default()
        };
        let resolved = resolve_editor(&cfg, &EditorFindKeybindings::default());
        assert_eq!(resolved.run_query, EditorBindings::default().run_query);
    }

    fn config_with_run_query_override() -> crate::config::Config {
        crate::config::Config {
            keybindings: KeybindingsConfig {
                editor: EditorKeybindings {
                    run_query: Some(keystrokes(&["f5"])),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_partial_editor_override_leaves_every_other_editor_action_at_its_default() {
        let resolved = resolve(&config_with_run_query_override());
        let default = EditorBindings::default();
        assert_eq!(resolved.editor.run_query, vec!["f5".to_owned()]);
        assert_eq!(resolved.editor.save_script, default.save_script);
        assert_eq!(resolved.editor.undo, default.undo);
        assert_eq!(resolved.editor.redo, default.redo);
        assert_eq!(resolved.editor.open_find, default.open_find);
        assert_eq!(resolved.editor.find_next, default.find_next);
        assert_eq!(resolved.editor.find_prev, default.find_prev);
        assert_eq!(resolved.editor.close_find, default.close_find);
    }

    #[test]
    fn a_toml_doc_overriding_only_editor_find_next_leaves_every_other_keybinding_at_its_default() {
        let config: crate::config::Config =
            toml::from_str("[keybindings.editor_find]\nnext = \"f7\"\n").unwrap();
        let resolved = resolve(&config);

        assert_eq!(resolved.editor.find_next, vec!["f7".to_owned()]);
        let editor_default = EditorBindings::default();
        assert_eq!(resolved.editor.open_find, editor_default.open_find);
        assert_eq!(resolved.editor.find_prev, editor_default.find_prev);
        assert_eq!(resolved.editor.close_find, editor_default.close_find);
        assert_eq!(resolved.editor.run_query, editor_default.run_query);
        assert_eq!(resolved.text_field, TextFieldBindings::default());
        assert_eq!(resolved.results, ResultsBindings::default());
        assert_eq!(resolved.value_panel, ValuePanelBindings::default());
        assert_eq!(resolved.sidebar, SidebarBindings::default());
        assert_eq!(resolved.schema_view, SchemaViewBindings::default());
        assert_eq!(resolved.open_modal, OpenModalBindings::default());
        assert_eq!(resolved.save_modal, SaveModalBindings::default());
    }

    #[test]
    fn a_partial_editor_override_leaves_every_other_area_at_its_defaults() {
        let resolved = resolve(&config_with_run_query_override());
        assert_eq!(resolved.text_field, TextFieldBindings::default());
        assert_eq!(resolved.results, ResultsBindings::default());
        assert_eq!(resolved.value_panel, ValuePanelBindings::default());
        assert_eq!(resolved.sidebar, SidebarBindings::default());
        assert_eq!(resolved.schema_view, SchemaViewBindings::default());
        assert_eq!(resolved.open_modal, OpenModalBindings::default());
        assert_eq!(resolved.save_modal, SaveModalBindings::default());
    }

    #[test]
    fn a_toml_doc_setting_only_editor_run_query_resolves_every_other_action_to_its_default() {
        let config: crate::config::Config =
            toml::from_str("[keybindings.editor]\nrun_query = \"f5\"\n").unwrap();
        let resolved = resolve(&config);

        assert_eq!(resolved.editor.run_query, vec!["f5".to_owned()]);
        let editor_default = EditorBindings::default();
        assert_eq!(resolved.editor.save_script, editor_default.save_script);
        assert_eq!(resolved.editor.undo, editor_default.undo);
        assert_eq!(resolved.editor.redo, editor_default.redo);
        assert_eq!(resolved.text_field, TextFieldBindings::default());
        assert_eq!(resolved.results, ResultsBindings::default());
        assert_eq!(resolved.value_panel, ValuePanelBindings::default());
        assert_eq!(resolved.sidebar, SidebarBindings::default());
        assert_eq!(resolved.schema_view, SchemaViewBindings::default());
        assert_eq!(resolved.open_modal, OpenModalBindings::default());
        assert_eq!(resolved.save_modal, SaveModalBindings::default());
    }

    #[test]
    fn a_toml_doc_overriding_the_manually_wired_fields_carries_each_to_its_own_output() {
        let config: crate::config::Config = toml::from_str(
            "[keybindings.quick_find]\nnext = \"f7\"\n\
             [keybindings.cell_edit]\ncancel = \"f8\"\n\
             [keybindings.sidebar]\nopen_find = \"f9\"\n",
        )
        .unwrap();
        let resolved = resolve(&config);

        assert_eq!(resolved.results.quick_find_next, vec!["f7".to_owned()]);
        assert_eq!(resolved.results.cancel_cell_edit, vec!["f8".to_owned()]);
        assert_eq!(resolved.sidebar.open_find, vec!["f9".to_owned()]);
        let results_default = ResultsBindings::default();
        assert_eq!(
            resolved.results.quick_find_prev,
            results_default.quick_find_prev
        );
        assert_eq!(
            resolved.results.quick_find_close,
            results_default.quick_find_close
        );
        assert_eq!(resolved.results.edit_cell, results_default.edit_cell);
        assert_eq!(
            resolved.sidebar.close_find,
            SidebarBindings::default().close_find
        );
    }

    #[test]
    fn legacy_staging_apply_keybinding_resolves_apply_staged_when_the_canonical_key_is_unset() {
        let config = crate::config::Config {
            staging: StagingConfig {
                apply_keybinding: "f5".to_owned(),
            },
            ..Default::default()
        };
        let resolved = resolve(&config);
        assert_eq!(resolved.results.apply_staged, vec!["f5".to_owned()]);
    }

    #[test]
    fn the_canonical_apply_staged_key_wins_over_the_legacy_staging_setting_when_both_are_set() {
        let config = crate::config::Config {
            keybindings: KeybindingsConfig {
                results: ResultsKeybindings {
                    apply_staged: Some(keystrokes(&["f6"])),
                    ..Default::default()
                },
                ..Default::default()
            },
            staging: StagingConfig {
                apply_keybinding: "f5".to_owned(),
            },
            ..Default::default()
        };
        let resolved = resolve(&config);
        assert_eq!(resolved.results.apply_staged, vec!["f6".to_owned()]);
    }

    #[test]
    fn an_invalid_canonical_apply_staged_falls_back_to_the_legacy_staging_setting() {
        let config = crate::config::Config {
            keybindings: KeybindingsConfig {
                results: ResultsKeybindings {
                    apply_staged: Some(keystrokes(&["not-a-key"])),
                    ..Default::default()
                },
                ..Default::default()
            },
            staging: StagingConfig {
                apply_keybinding: "f5".to_owned(),
            },
            ..Default::default()
        };
        let resolved = resolve(&config);
        assert_eq!(resolved.results.apply_staged, vec!["f5".to_owned()]);
    }

    #[test]
    fn an_invalid_legacy_staging_keybinding_falls_back_to_the_hardcoded_default() {
        let config = crate::config::Config {
            staging: StagingConfig {
                apply_keybinding: "not-a-key".to_owned(),
            },
            ..Default::default()
        };
        let resolved = resolve(&config);
        assert_eq!(
            resolved.results.apply_staged,
            ResultsBindings::default().apply_staged
        );
    }

    // -- wiring: resolving a default config touches every area's own
    // `Default` impl and nothing else -- see the literal pins below for the
    // actual keystroke values these `Default` impls must hold.

    #[test]
    fn resolving_a_default_config_wires_the_editor_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.editor, EditorBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_text_field_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.text_field, TextFieldBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_results_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.results, ResultsBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_value_panel_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.value_panel, ValuePanelBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_sidebar_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.sidebar, SidebarBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_schema_view_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.schema_view, SchemaViewBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_open_modal_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.open_modal, OpenModalBindings::default());
    }

    #[test]
    fn resolving_a_default_config_wires_the_save_modal_areas_own_default_impl() {
        let resolved = resolve(&crate::config::Config::default());
        assert_eq!(resolved.save_modal, SaveModalBindings::default());
    }

    // -- pinning: resolved defaults match today's hardcoded keystrokes,
    // asserted literally so a typo in an area's `Default` impl (e.g.
    // "secondry-c") is caught even though it would still equal itself in
    // the wiring checks above.

    fn one(s: &str) -> Vec<String> {
        vec![s.to_owned()]
    }

    #[test]
    fn editor_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).editor;
        assert_eq!(d.move_left, one("left"));
        assert_eq!(d.move_right, one("right"));
        assert_eq!(d.move_up, one("up"));
        assert_eq!(d.move_down, one("down"));
        assert_eq!(d.move_line_start, one("home"));
        assert_eq!(d.move_line_end, one("end"));
        assert_eq!(d.move_document_start, one("secondary-up"));
        assert_eq!(d.move_document_end, one("secondary-down"));
        assert_eq!(d.select_left, one("shift-left"));
        assert_eq!(d.select_right, one("shift-right"));
        assert_eq!(d.select_up, one("shift-up"));
        assert_eq!(d.select_down, one("shift-down"));
        assert_eq!(d.select_line_start, one("shift-home"));
        assert_eq!(d.select_line_end, one("shift-end"));
        assert_eq!(d.select_document_start, one("shift-secondary-up"));
        assert_eq!(d.select_document_end, one("shift-secondary-down"));
        assert_eq!(d.select_all, one("secondary-a"));
        assert_eq!(d.backspace, one("backspace"));
        assert_eq!(d.delete_forward, one("delete"));
        assert_eq!(d.newline, one("enter"));
        assert_eq!(d.copy, one("secondary-c"));
        assert_eq!(d.cut, one("secondary-x"));
        assert_eq!(d.paste, one("secondary-v"));
        assert_eq!(
            d.run_query,
            vec!["cmd-enter".to_owned(), "ctrl-enter".to_owned()]
        );
        assert_eq!(d.save_script, one("secondary-s"));
        assert_eq!(d.save_script_as, one("shift-secondary-s"));
        assert_eq!(d.open_script, one("secondary-o"));
        assert_eq!(d.browse_script_files, one("shift-secondary-o"));
        assert_eq!(d.undo, one("secondary-z"));
        assert_eq!(
            d.redo,
            vec![
                "shift-secondary-z".to_owned(),
                "secondary-y".to_owned(),
                "ctrl-y".to_owned(),
            ]
        );
        assert_eq!(d.open_find, one("secondary-f"));
        assert_eq!(d.find_next, one("enter"));
        assert_eq!(d.find_prev, one("shift-enter"));
        assert_eq!(d.close_find, one("escape"));
    }

    #[test]
    fn text_field_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).text_field;
        assert_eq!(d.move_left, one("left"));
        assert_eq!(d.move_right, one("right"));
        assert_eq!(d.move_home, one("home"));
        assert_eq!(d.move_end, one("end"));
        assert_eq!(d.select_left, one("shift-left"));
        assert_eq!(d.select_right, one("shift-right"));
        assert_eq!(d.select_home, one("shift-home"));
        assert_eq!(d.select_end, one("shift-end"));
        assert_eq!(d.select_all, one("secondary-a"));
        assert_eq!(d.backspace, one("backspace"));
        assert_eq!(d.delete_forward, one("delete"));
        assert_eq!(d.submit, one("enter"));
        assert_eq!(d.copy, one("secondary-c"));
        assert_eq!(d.cut, one("secondary-x"));
        assert_eq!(d.paste, one("secondary-v"));
    }

    #[test]
    fn results_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).results;
        assert_eq!(d.copy, one("secondary-c"));
        assert_eq!(d.cell_up, one("up"));
        assert_eq!(d.cell_down, one("down"));
        assert_eq!(d.cell_left, one("left"));
        assert_eq!(d.cell_right, one("right"));
        assert_eq!(d.toggle_value_panel, one("space"));
        assert_eq!(d.close_value_panel, one("escape"));
        assert_eq!(d.focus_value_panel, one("tab"));
        assert_eq!(d.prev_page, one("ctrl-["));
        assert_eq!(d.next_page, one("ctrl-]"));
        assert_eq!(d.open_quick_find, one("secondary-f"));
        assert_eq!(d.apply_staged, one("ctrl-shift-enter"));
        assert_eq!(d.edit_cell, one("f2"));
        assert_eq!(
            d.quick_find_prev,
            vec!["shift-enter".to_owned(), "up".to_owned()]
        );
        assert_eq!(d.quick_find_next, one("down"));
        assert_eq!(d.quick_find_close, one("escape"));
        assert_eq!(d.cancel_cell_edit, one("escape"));
    }

    #[test]
    fn value_panel_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).value_panel;
        assert_eq!(d.tree_up, one("up"));
        assert_eq!(d.tree_down, one("down"));
        assert_eq!(d.tree_collapse, one("left"));
        assert_eq!(d.tree_expand, one("right"));
        assert_eq!(d.copy_tree_node_value, one("secondary-c"));
        assert_eq!(d.copy_tree_node_path, one("shift-secondary-c"));
        assert_eq!(d.close_panel_from_panel, one("escape"));
        assert_eq!(d.focus_grid_from_panel, one("tab"));
    }

    #[test]
    fn sidebar_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).sidebar;
        assert_eq!(d.open_find, one("secondary-f"));
        assert_eq!(d.close_find, one("escape"));
    }

    #[test]
    fn schema_view_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).schema_view;
        assert_eq!(d.copy, one("secondary-c"));
    }

    #[test]
    fn open_modal_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).open_modal;
        assert_eq!(d.select_previous_row, one("up"));
        assert_eq!(d.select_next_row, one("down"));
    }

    #[test]
    fn save_modal_defaults_pin_every_action_to_its_historical_keystroke() {
        let d = resolve(&crate::config::Config::default()).save_modal;
        assert_eq!(d.select_previous_destination, one("up"));
        assert_eq!(d.select_next_destination, one("down"));
    }
}
