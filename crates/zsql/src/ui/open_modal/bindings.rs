//! [`OpenModalBindings`]: one keystroke list per Open Script picker action.

/// One keystroke list per Open Script picker action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenModalBindings {
    pub select_previous_row: Vec<String>,
    pub select_next_row: Vec<String>,
}

impl Default for OpenModalBindings {
    fn default() -> Self {
        Self {
            select_previous_row: vec!["up".to_owned()],
            select_next_row: vec!["down".to_owned()],
        }
    }
}
