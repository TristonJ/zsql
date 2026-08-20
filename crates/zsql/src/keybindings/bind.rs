use gpui::{Action, KeyBinding};

/// Push one `KeyBinding` per entry in `keystrokes` for `action`, scoped to
/// `context`.
pub(crate) fn bind_all(
    keys: &mut Vec<KeyBinding>,
    keystrokes: &[String],
    action: &(impl Action + Clone),
    context: &'static str,
) {
    for keystroke in keystrokes {
        keys.push(KeyBinding::new(
            keystroke,
            Clone::clone(action),
            Some(context),
        ));
    }
}
