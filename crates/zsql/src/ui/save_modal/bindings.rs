//! [`SaveModalBindings`]: one keystroke list per Save Script modal action.

/// One keystroke list per Save Script modal action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveModalBindings {
    pub select_previous_destination: Vec<String>,
    pub select_next_destination: Vec<String>,
}

impl Default for SaveModalBindings {
    fn default() -> Self {
        Self {
            select_previous_destination: vec!["up".to_owned()],
            select_next_destination: vec!["down".to_owned()],
        }
    }
}
