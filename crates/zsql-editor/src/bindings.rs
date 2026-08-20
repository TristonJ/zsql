//! [`EditorBindings`]: one keystroke list per editor action, so an
//! embedding app can override any of them without this crate knowing
//! anything about a config file.

/// One keystroke list per editor action. An action with more than one
/// default keystroke (e.g. [`crate::RunQuery`]) registers one `KeyBinding`
/// per entry in its list. Entries use gpui keystroke syntax; the
/// `secondary-` prefix is the platform's primary modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorBindings {
    pub move_left: Vec<String>,
    pub move_right: Vec<String>,
    pub move_up: Vec<String>,
    pub move_down: Vec<String>,
    pub move_line_start: Vec<String>,
    pub move_line_end: Vec<String>,
    pub move_document_start: Vec<String>,
    pub move_document_end: Vec<String>,
    pub select_left: Vec<String>,
    pub select_right: Vec<String>,
    pub select_up: Vec<String>,
    pub select_down: Vec<String>,
    pub select_line_start: Vec<String>,
    pub select_line_end: Vec<String>,
    pub select_document_start: Vec<String>,
    pub select_document_end: Vec<String>,
    pub select_all: Vec<String>,
    pub backspace: Vec<String>,
    pub delete_forward: Vec<String>,
    pub newline: Vec<String>,
    pub copy: Vec<String>,
    pub cut: Vec<String>,
    pub paste: Vec<String>,
    pub run_query: Vec<String>,
    pub save_script: Vec<String>,
    pub save_script_as: Vec<String>,
    pub open_script: Vec<String>,
    pub browse_script_files: Vec<String>,
    pub undo: Vec<String>,
    pub redo: Vec<String>,
}

fn one(s: &str) -> Vec<String> {
    vec![s.to_owned()]
}

impl Default for EditorBindings {
    fn default() -> Self {
        Self {
            move_left: one("left"),
            move_right: one("right"),
            move_up: one("up"),
            move_down: one("down"),
            move_line_start: one("home"),
            move_line_end: one("end"),
            move_document_start: one("secondary-up"),
            move_document_end: one("secondary-down"),
            select_left: one("shift-left"),
            select_right: one("shift-right"),
            select_up: one("shift-up"),
            select_down: one("shift-down"),
            select_line_start: one("shift-home"),
            select_line_end: one("shift-end"),
            select_document_start: one("shift-secondary-up"),
            select_document_end: one("shift-secondary-down"),
            select_all: one("secondary-a"),
            backspace: one("backspace"),
            delete_forward: one("delete"),
            newline: one("enter"),
            copy: one("secondary-c"),
            cut: one("secondary-x"),
            paste: one("secondary-v"),
            run_query: vec!["cmd-enter".to_owned(), "ctrl-enter".to_owned()],
            save_script: one("secondary-s"),
            save_script_as: one("shift-secondary-s"),
            open_script: one("secondary-o"),
            browse_script_files: one("shift-secondary-o"),
            undo: one("secondary-z"),
            redo: vec![
                "shift-secondary-z".to_owned(),
                "secondary-y".to_owned(),
                "ctrl-y".to_owned(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EditorBindings;

    #[test]
    fn run_query_defaults_to_both_cmd_enter_and_ctrl_enter() {
        assert_eq!(
            EditorBindings::default().run_query,
            vec!["cmd-enter".to_owned(), "ctrl-enter".to_owned()]
        );
    }

    #[test]
    fn redo_defaults_to_all_three_historical_chords() {
        assert_eq!(
            EditorBindings::default().redo,
            vec![
                "shift-secondary-z".to_owned(),
                "secondary-y".to_owned(),
                "ctrl-y".to_owned(),
            ]
        );
    }
}
