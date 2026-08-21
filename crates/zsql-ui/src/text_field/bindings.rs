//! [`TextFieldBindings`]: one keystroke list per `TextField` action, so an
//! embedding app can override any of them without this crate knowing
//! anything about a config file.

/// One keystroke list per `TextField` action. Entries use gpui keystroke
/// syntax; the `secondary-` prefix is the platform's primary modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFieldBindings {
    pub move_left: Vec<String>,
    pub move_right: Vec<String>,
    pub move_home: Vec<String>,
    pub move_end: Vec<String>,
    pub select_left: Vec<String>,
    pub select_right: Vec<String>,
    pub select_home: Vec<String>,
    pub select_end: Vec<String>,
    pub select_all: Vec<String>,
    pub backspace: Vec<String>,
    pub delete_forward: Vec<String>,
    pub submit: Vec<String>,
    pub copy: Vec<String>,
    pub cut: Vec<String>,
    pub paste: Vec<String>,
}

fn one(s: &str) -> Vec<String> {
    vec![s.to_owned()]
}

impl Default for TextFieldBindings {
    fn default() -> Self {
        Self {
            move_left: one("left"),
            move_right: one("right"),
            move_home: one("home"),
            move_end: one("end"),
            select_left: one("shift-left"),
            select_right: one("shift-right"),
            select_home: one("shift-home"),
            select_end: one("shift-end"),
            select_all: one("secondary-a"),
            backspace: one("backspace"),
            delete_forward: one("delete"),
            submit: one("enter"),
            copy: one("secondary-c"),
            cut: one("secondary-x"),
            paste: one("secondary-v"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TextFieldBindings;

    #[test]
    fn submit_defaults_to_enter() {
        assert_eq!(
            TextFieldBindings::default().submit,
            vec!["enter".to_owned()]
        );
    }
}
