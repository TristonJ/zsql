//! [`ValuePanelBindings`]: one keystroke list per value-panel action.

/// One keystroke list per value-panel action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValuePanelBindings {
    pub tree_up: Vec<String>,
    pub tree_down: Vec<String>,
    pub tree_collapse: Vec<String>,
    pub tree_expand: Vec<String>,
    pub copy_tree_node_value: Vec<String>,
    pub copy_tree_node_path: Vec<String>,
    pub close_panel_from_panel: Vec<String>,
    pub focus_grid_from_panel: Vec<String>,
}

fn one(s: &str) -> Vec<String> {
    vec![s.to_owned()]
}

impl Default for ValuePanelBindings {
    fn default() -> Self {
        Self {
            tree_up: one("up"),
            tree_down: one("down"),
            tree_collapse: one("left"),
            tree_expand: one("right"),
            copy_tree_node_value: one("secondary-c"),
            copy_tree_node_path: one("shift-secondary-c"),
            close_panel_from_panel: one("escape"),
            focus_grid_from_panel: one("tab"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ValuePanelBindings;

    #[test]
    fn copy_tree_node_path_defaults_to_shift_secondary_c() {
        assert_eq!(
            ValuePanelBindings::default().copy_tree_node_path,
            vec!["shift-secondary-c".to_owned()]
        );
    }
}
