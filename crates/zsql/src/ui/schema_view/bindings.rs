//! [`SchemaViewBindings`]: one keystroke list per schema-tab action.

/// One keystroke list per schema-tab action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViewBindings {
    pub copy: Vec<String>,
}

impl Default for SchemaViewBindings {
    fn default() -> Self {
        Self {
            copy: vec!["secondary-c".to_owned()],
        }
    }
}
