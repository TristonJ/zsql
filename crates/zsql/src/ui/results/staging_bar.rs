//! The staging bar: docks above the status bar while the staged-changes
//! queue is non-empty.

use gpui::{
    App, ClickEvent, Div, KeyContext, Modifiers, RenderOnce, Stateful, Window, div, prelude::*, px,
    rgb,
};
use zsql_ui::theme::{ActiveTheme, Colors, Theme};

use super::ApplyStagedChanges;
use crate::ui::theme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The bar docked above the status bar while the staged-changes queue is
/// non-empty.
#[derive(IntoElement)]
pub(super) struct StagingBar {
    edit_count: usize,
    delete_count: usize,
    ledger_open: bool,
    retrying: bool,
    applying: bool,
    /// A failure message not attached to any specific ledger line (e.g. no
    /// connection, or the transaction's own `BEGIN`/`COMMIT` failed).
    general_error: Option<String>,
    on_toggle_ledger: Option<ClickHandler>,
    on_discard_all: Option<ClickHandler>,
    on_apply: Option<ClickHandler>,
}

impl StagingBar {
    pub(super) fn new(edit_count: usize, delete_count: usize, ledger_open: bool) -> Self {
        Self {
            edit_count,
            delete_count,
            ledger_open,
            retrying: false,
            applying: false,
            general_error: None,
            on_toggle_ledger: None,
            on_discard_all: None,
            on_apply: None,
        }
    }

    /// Whether the previous Apply failed.
    #[must_use]
    pub(super) fn retrying(mut self, retrying: bool) -> Self {
        self.retrying = retrying;
        self
    }

    /// Whether an Apply is currently in flight.
    #[must_use]
    pub(super) fn applying(mut self, applying: bool) -> Self {
        self.applying = applying;
        self
    }

    /// A failure message not attached to any specific ledger line. `None`
    /// clears it.
    #[must_use]
    pub(super) fn general_error(mut self, error: Option<String>) -> Self {
        self.general_error = error;
        self
    }

    #[must_use]
    pub(super) fn on_toggle_ledger(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_ledger = Some(Box::new(f));
        self
    }

    #[must_use]
    pub(super) fn on_discard_all(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_discard_all = Some(Box::new(f));
        self
    }

    #[must_use]
    pub(super) fn on_apply(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_apply = Some(Box::new(f));
        self
    }
}

/// "1 delete" / "2 deletes".
fn delete_count_label(count: usize) -> String {
    if count == 1 {
        "1 delete".to_owned()
    } else {
        format!("{count} deletes")
    }
}

/// "1 edit" / "2 edits".
fn edit_count_label(count: usize) -> String {
    if count == 1 {
        "1 edit".to_owned()
    } else {
        format!("{count} edits")
    }
}

/// The bar's "n edit(s) \u{b7} n delete(s)" summary.
fn render_summary(edit_count: usize, delete_count: usize, colors: Colors) -> Div {
    let mut summary = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .text_size(px(theme::STAGING_BAR_TEXT_SIZE));
    if edit_count > 0 {
        summary = summary.child(
            div()
                .text_color(rgb(colors.status_warn))
                .child(edit_count_label(edit_count)),
        );
    }
    if edit_count > 0 && delete_count > 0 {
        summary = summary.child(div().text_color(rgb(colors.text_tertiary)).child("\u{b7}"));
    }
    if delete_count > 0 {
        summary = summary.child(
            div()
                .text_color(rgb(colors.status_error))
                .child(delete_count_label(delete_count)),
        );
    }
    summary
}

fn render_ledger_toggle(
    ledger_open: bool,
    colors: Colors,
    on_toggle: Option<ClickHandler>,
) -> Stateful<Div> {
    let label = if ledger_open {
        "hide sql"
    } else {
        "review sql"
    };
    let mut el = div()
        .id("staging-bar-review-sql")
        .debug_selector(|| "staging-bar-review-sql".to_owned())
        .cursor_pointer()
        .text_size(px(theme::STAGING_BAR_TEXT_SIZE))
        .text_color(rgb(colors.text_secondary))
        .hover(|el| el.text_color(rgb(colors.text_primary)))
        .child(label);
    if let Some(on_toggle) = on_toggle {
        el = el.on_click(on_toggle);
    }
    el
}

fn render_discard_all(theme: &Theme, on_discard: Option<ClickHandler>) -> Stateful<Div> {
    let colors = theme.colors;
    let mut el = div()
        .id("staging-bar-discard-all")
        .debug_selector(|| "staging-bar-discard-all".to_owned())
        .cursor_pointer()
        .h(theme::STAGING_BUTTON_HEIGHT)
        .px(theme::STAGING_BUTTON_PADDING_X)
        .flex()
        .items_center()
        .rounded(px(theme::STAGING_BUTTON_RADIUS))
        .border_1()
        .border_color(rgb(colors.border))
        .text_size(theme::STAGING_BUTTON_TEXT_SIZE)
        .text_color(rgb(colors.text_secondary))
        .font_family(theme.fonts.ui.clone())
        .hover(|el| {
            el.bg(rgb(colors.bg_overlay))
                .text_color(rgb(colors.text_primary))
        })
        .child("Discard all");
    if let Some(on_discard) = on_discard {
        el = el.on_click(on_discard);
    }
    el
}

/// The bar's leading tag: amber "STAGED", or rose "APPLY FAILED" after a
/// failed Apply.
fn tag_label(retrying: bool) -> &'static str {
    if retrying { "APPLY FAILED" } else { "STAGED" }
}

fn render_tag(retrying: bool, theme: &Theme) -> Div {
    let colors = theme.colors;
    let (accent, outline) = if retrying {
        (colors.status_error, colors.error_outline())
    } else {
        (colors.status_warn, colors.warn_outline())
    };
    div()
        .text_size(theme::STAGING_TAG_TEXT_SIZE)
        .font_family(theme.fonts.data.clone())
        .px(theme::STAGING_TAG_PADDING_X)
        .rounded(px(theme::STAGING_TAG_RADIUS))
        .border_1()
        .border_color(outline)
        .bg(Colors::wash(accent, 0x14))
        .text_color(rgb(accent))
        .child(tag_label(retrying))
}

/// "Apply n" / "Retry n", for the Apply control's label.
fn apply_label(count: usize, retrying: bool) -> String {
    if retrying {
        format!("Retry {count}")
    } else {
        format!("Apply {count}")
    }
}

/// The Apply control's keybinding hint, read from whatever chord is
/// actually bound to [`ApplyStagedChanges`] so the hint can never drift
/// from the binding.
fn apply_keybinding_hint(window: &Window) -> Option<String> {
    let context = KeyContext::parse(crate::ui::results::KEY_CONTEXT).ok()?;
    let binding = window
        .bindings_for_action_in_context(&ApplyStagedChanges, context)
        .pop()?;
    Some(
        binding
            .keystrokes()
            .iter()
            .map(|keystroke| keystroke_label(*keystroke.modifiers(), keystroke.key()))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// One keystroke as hint text, e.g. `Ctrl+Shift+Enter`.
fn keystroke_label(modifiers: Modifiers, key: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (held, name) in [
        (modifiers.control, "Ctrl"),
        (modifiers.alt, "Alt"),
        (modifiers.shift, "Shift"),
        (modifiers.platform, "Super"),
        (modifiers.function, "Fn"),
    ] {
        if held {
            parts.push(name.to_owned());
        }
    }
    let mut key_text = String::new();
    let mut chars = key.chars();
    if let Some(first) = chars.next() {
        key_text.extend(first.to_uppercase());
        key_text.push_str(chars.as_str());
    }
    parts.push(key_text);
    parts.join("+")
}

fn render_apply(
    count: usize,
    retrying: bool,
    applying: bool,
    hint: Option<String>,
    theme: &Theme,
    on_apply: Option<ClickHandler>,
) -> Stateful<Div> {
    let colors = theme.colors;
    let label = apply_label(count, retrying);
    let mut el = div()
        .id("staging-bar-apply")
        .debug_selector(|| "staging-bar-apply".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .h(theme::STAGING_BUTTON_HEIGHT)
        .px(theme::STAGING_BUTTON_PADDING_X)
        .rounded(px(theme::STAGING_BUTTON_RADIUS))
        .border_1()
        .border_color(colors.warn_outline())
        .bg(Colors::wash(colors.status_warn, 0x24))
        .text_size(theme::STAGING_BUTTON_TEXT_SIZE)
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(colors.text_primary))
        .font_family(theme.fonts.ui.clone())
        .child(label)
        .children(hint.map(|hint| {
            div()
                .text_size(px(theme::STAGING_APPLY_HINT_TEXT_SIZE))
                .text_color(rgb(colors.status_warn))
                .child(hint)
        }));
    if applying {
        el = el.opacity(theme::PAGER_DISABLED_OPACITY);
    } else {
        el = el.cursor_pointer();
        if let Some(on_apply) = on_apply {
            el = el.on_click(on_apply);
        }
    }
    el
}

/// The bar's error strip, shown below its main row while a failure is not
/// attached to any specific ledger line.
fn render_general_error(message: String, colors: Colors) -> Stateful<Div> {
    div()
        .id("staging-bar-error")
        .debug_selector(|| "staging-bar-error".to_owned())
        .px_3()
        .pb_1()
        .text_size(px(theme::STAGING_BAR_TEXT_SIZE))
        .text_color(rgb(colors.status_error))
        .child(message)
}

impl RenderOnce for StagingBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let apply_hint = apply_keybinding_hint(window);
        let theme = cx.theme();
        let colors = theme.colors;

        let main_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .h(theme::RESULTS_BAR_HEIGHT)
            .px_3()
            .child(render_tag(self.retrying, theme))
            .child(render_summary(self.edit_count, self.delete_count, colors))
            .child(render_ledger_toggle(
                self.ledger_open,
                colors,
                self.on_toggle_ledger,
            ))
            .child(div().flex_1())
            .child(render_discard_all(theme, self.on_discard_all))
            .child(render_apply(
                self.edit_count + self.delete_count,
                self.retrying,
                self.applying,
                apply_hint,
                theme,
                self.on_apply,
            ));

        div()
            .id("staging-bar")
            .debug_selector(|| "staging-bar".to_owned())
            .flex()
            .flex_col()
            .flex_shrink_0()
            .bg(rgb(colors.bg_raised))
            .border_t_1()
            .border_color(colors.warn_outline())
            .font_family(&theme.fonts.data)
            .child(main_row)
            .children(
                self.general_error
                    .map(|message| render_general_error(message, colors)),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::Modifiers;

    use super::{apply_label, delete_count_label, edit_count_label, keystroke_label, tag_label};

    #[test]
    fn the_tag_reads_staged_until_an_apply_fails() {
        assert_eq!(tag_label(false), "STAGED");
        assert_eq!(tag_label(true), "APPLY FAILED");
    }

    #[test]
    fn keystroke_label_names_each_held_modifier_and_capitalizes_the_key() {
        let modifiers = Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(keystroke_label(modifiers, "enter"), "Ctrl+Shift+Enter");
    }

    #[test]
    fn keystroke_label_renders_a_bare_key_without_separators() {
        assert_eq!(keystroke_label(Modifiers::default(), "f5"), "F5");
    }

    #[test]
    fn delete_count_label_is_singular_for_exactly_one() {
        assert_eq!(delete_count_label(1), "1 delete");
    }

    #[test]
    fn delete_count_label_is_plural_for_any_other_count() {
        assert_eq!(delete_count_label(0), "0 deletes");
        assert_eq!(delete_count_label(2), "2 deletes");
    }

    #[test]
    fn edit_count_label_is_singular_for_exactly_one() {
        assert_eq!(edit_count_label(1), "1 edit");
    }

    #[test]
    fn edit_count_label_is_plural_for_any_other_count() {
        assert_eq!(edit_count_label(0), "0 edits");
        assert_eq!(edit_count_label(2), "2 edits");
    }

    #[test]
    fn apply_label_reads_apply_n_when_not_retrying() {
        assert_eq!(apply_label(3, false), "Apply 3");
    }

    #[test]
    fn apply_label_reads_retry_n_after_a_failed_apply() {
        assert_eq!(apply_label(3, true), "Retry 3");
    }
}
