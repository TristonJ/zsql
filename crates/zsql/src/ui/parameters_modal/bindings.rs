//! [`ParametersModalBindings`]: one keystroke list per "Run with
//! parameters" modal action.

/// One keystroke list per "Run with parameters" modal action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParametersModalBindings {
    pub next_field: Vec<String>,
    pub previous_field: Vec<String>,
}

impl Default for ParametersModalBindings {
    fn default() -> Self {
        Self {
            next_field: vec!["tab".to_owned()],
            previous_field: vec!["shift-tab".to_owned()],
        }
    }
}
