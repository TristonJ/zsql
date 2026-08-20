//! [`ResultsBindings`]: one keystroke list per results-grid action, plus
//! the quick-find row and cell-edit popover actions that share its key
//! context.

/// One keystroke list per results-grid action, plus the quick-find and
/// cell-edit popover actions that share its key context. `apply_staged`
/// arrives already resolved against the legacy `staging.apply_keybinding`
/// fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultsBindings {
    pub copy: Vec<String>,
    pub cell_up: Vec<String>,
    pub cell_down: Vec<String>,
    pub cell_left: Vec<String>,
    pub cell_right: Vec<String>,
    pub toggle_value_panel: Vec<String>,
    pub close_value_panel: Vec<String>,
    pub focus_value_panel: Vec<String>,
    pub prev_page: Vec<String>,
    pub next_page: Vec<String>,
    pub open_quick_find: Vec<String>,
    pub apply_staged: Vec<String>,
    pub edit_cell: Vec<String>,
    pub quick_find_prev: Vec<String>,
    pub quick_find_next: Vec<String>,
    pub quick_find_close: Vec<String>,
    pub cancel_cell_edit: Vec<String>,
}

fn one(s: &str) -> Vec<String> {
    vec![s.to_owned()]
}

impl Default for ResultsBindings {
    fn default() -> Self {
        Self {
            copy: one("secondary-c"),
            cell_up: one("up"),
            cell_down: one("down"),
            cell_left: one("left"),
            cell_right: one("right"),
            toggle_value_panel: one("space"),
            close_value_panel: one("escape"),
            focus_value_panel: one("tab"),
            prev_page: one("ctrl-["),
            next_page: one("ctrl-]"),
            open_quick_find: one("secondary-f"),
            apply_staged: one("ctrl-shift-enter"),
            edit_cell: one("f2"),
            quick_find_prev: vec!["shift-enter".to_owned(), "up".to_owned()],
            quick_find_next: one("down"),
            quick_find_close: one("escape"),
            cancel_cell_edit: one("escape"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ResultsBindings;

    #[test]
    fn apply_staged_defaults_to_ctrl_shift_enter() {
        assert_eq!(
            ResultsBindings::default().apply_staged,
            vec!["ctrl-shift-enter".to_owned()]
        );
    }

    #[test]
    fn quick_find_prev_defaults_to_shift_enter_and_up() {
        assert_eq!(
            ResultsBindings::default().quick_find_prev,
            vec!["shift-enter".to_owned(), "up".to_owned()]
        );
    }
}
