//! [`SidebarBindings`]: one keystroke list per sidebar action.

/// One keystroke list per sidebar action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarBindings {
    pub open_find: Vec<String>,
    pub close_find: Vec<String>,
}

fn one(s: &str) -> Vec<String> {
    vec![s.to_owned()]
}

impl Default for SidebarBindings {
    fn default() -> Self {
        Self {
            open_find: one("secondary-f"),
            close_find: one("escape"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SidebarBindings;

    #[test]
    fn open_find_defaults_to_secondary_f() {
        assert_eq!(
            SidebarBindings::default().open_find,
            vec!["secondary-f".to_owned()]
        );
    }
}
