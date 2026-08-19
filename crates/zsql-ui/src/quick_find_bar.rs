//! The quick-find bar: a self-contained search pill (query input, live
//! match counter, previous/next, case toggle, close) that any view can
//! overlay on its own content. The bar owns only its UI state and emits
//! [`QuickFindBarEvent`]s; the host owns matching (usually via
//! [`crate::quick_find::QuickFind`]), highlight rendering, key bindings,
//! and placement, so the same bar serves any searchable context.

use gpui::{
    ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Stateful,
    Window, div, prelude::*, px, rgb,
};

use crate::text_field::{TextFieldEvent, TextFieldState, TextFieldStyle};
use crate::theme::{ActiveTheme, Colors};

/// Height of the bar.
pub const QUICK_FIND_BAR_HEIGHT: gpui::Pixels = px(34.0);
/// Horizontal padding inside the bar.
const BAR_PADDING_X: gpui::Pixels = px(7.0);
/// Gap between the bar's children.
const BAR_GAP: gpui::Pixels = px(6.0);
/// Corner radius of the bar.
const BAR_RADIUS: f32 = 7.0;
/// Width of the query input.
const INPUT_WIDTH: gpui::Pixels = px(168.0);
/// Text size of the match counter.
const COUNTER_TEXT_SIZE: f32 = 11.0;
/// Width and height of each square button.
const BUTTON_SIZE: gpui::Pixels = px(24.0);
/// Corner radius of each button.
const BUTTON_RADIUS: f32 = 5.0;
/// Text size of the arrow and close buttons.
const BUTTON_TEXT_SIZE: f32 = 12.0;
/// Text size of the "Aa" case toggle.
const AA_TEXT_SIZE: f32 = 10.5;

/// What the bar asks its host to do. The bar never mutates match state
/// itself; the host reacts and pushes the outcome back via
/// [`QuickFindBar::set_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickFindBarEvent {
    /// The query text changed; recompute matches against it.
    QueryChanged(String),
    /// Step to the next (`forward`) or previous match, wrapping at the ends.
    StepRequested {
        /// Step direction: `true` walks toward later matches.
        forward: bool,
    },
    /// Flip case-sensitive matching and recompute.
    CaseToggleRequested,
    /// Close the bar and clear every highlight.
    DismissRequested,
}

/// The bar's view state: its query input plus the display status the host
/// pushed last (match position and case mode).
pub struct QuickFindBar {
    id: SharedString,
    input: Entity<TextFieldState>,
    last_query: String,
    current: usize,
    total: usize,
    case_on: bool,
}

impl EventEmitter<QuickFindBarEvent> for QuickFindBar {}

impl QuickFindBar {
    /// A bar identified by `id` (unique per instance, since it names element
    /// ids) whose input shows `placeholder` while empty.
    pub fn new(
        id: impl Into<SharedString>,
        placeholder: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextFieldState::new(placeholder, None, cx).style(TextFieldStyle {
                height: BUTTON_SIZE,
                ..Default::default()
            })
        });
        cx.subscribe(&input, |_bar, _field, event, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                cx.emit(QuickFindBarEvent::StepRequested { forward: true });
            }
        })
        .detach();
        // Emits only when the query text actually changed: the field also
        // notifies on visual changes (its caret blink loop ticks while
        // focused), and re-emitting the same query would make the host reset
        // the current match on every tick.
        cx.observe(&input, |bar, field, cx| {
            let value = field.read(cx).value().to_string();
            if bar.last_query != value {
                bar.last_query.clone_from(&value);
                cx.emit(QuickFindBarEvent::QueryChanged(value));
            }
        })
        .detach();
        Self {
            id: id.into(),
            input,
            last_query: String::new(),
            current: 0,
            total: 0,
            case_on: false,
        }
    }

    /// Update the display status: the current match's 1-based number (0 when
    /// none), the total match count, and whether case-sensitive matching is
    /// armed.
    pub fn set_status(
        &mut self,
        current: usize,
        total: usize,
        case_on: bool,
        cx: &mut Context<Self>,
    ) {
        self.current = current;
        self.total = total;
        self.case_on = case_on;
        cx.notify();
    }

    /// Move window focus into the query input.
    pub fn focus_input(&self, window: &mut Window, cx: &gpui::App) {
        window.focus(&self.input.read(cx).focus_handle(cx));
    }

    /// The query input's focus handle.
    #[must_use]
    pub fn input_focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    /// The query input, for hosts and tests that drive it directly.
    #[must_use]
    pub fn input(&self) -> &Entity<TextFieldState> {
        &self.input
    }

    /// This instance's element id with `suffix` appended, keeping every
    /// interactive child's id unique across bar instances.
    fn child_id(&self, suffix: &str) -> SharedString {
        SharedString::from(format!("{}-{suffix}", self.id))
    }

    /// A square arrow button stepping in `forward`'s direction.
    fn step_button(&self, forward: bool, colors: &Colors, cx: &mut Context<Self>) -> Stateful<Div> {
        let (suffix, glyph) = if forward {
            ("next", "\u{2193}")
        } else {
            ("prev", "\u{2191}")
        };
        let tag = self.child_id(suffix);
        div()
            .id(tag.clone())
            .debug_selector(move || tag.to_string())
            .flex()
            .items_center()
            .justify_center()
            .w(BUTTON_SIZE)
            .h(BUTTON_SIZE)
            .rounded(px(BUTTON_RADIUS))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.bg_panel))
            .text_size(px(BUTTON_TEXT_SIZE))
            .text_color(rgb(colors.text_secondary))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .child(glyph)
            .on_click(cx.listener(move |_bar, _event: &ClickEvent, _window, cx| {
                cx.emit(QuickFindBarEvent::StepRequested { forward });
            }))
    }

    /// The "Aa" case-sensitivity toggle, accent-washed while armed.
    fn case_toggle(&self, colors: &Colors, cx: &mut Context<Self>) -> Stateful<Div> {
        let tag = self.child_id("case");
        let button = div()
            .id(tag.clone())
            .debug_selector(move || tag.to_string())
            .flex()
            .items_center()
            .justify_center()
            .w(BUTTON_SIZE)
            .h(BUTTON_SIZE)
            .rounded(px(BUTTON_RADIUS))
            .border_1()
            .text_size(px(AA_TEXT_SIZE))
            .cursor_pointer()
            .child("Aa");

        if self.case_on {
            button
                .border_color(colors.accent_outline())
                .bg(colors.accent_wash_soft())
                .text_color(rgb(colors.accent))
        } else {
            button
                .border_color(rgb(colors.border))
                .bg(rgb(colors.bg_panel))
                .text_color(rgb(colors.text_secondary))
                .hover(|el| el.bg(rgb(colors.bg_raised)))
        }
        .on_click(cx.listener(|_bar, _event: &ClickEvent, _window, cx| {
            cx.emit(QuickFindBarEvent::CaseToggleRequested);
        }))
    }

    fn close_button(&self, colors: &Colors, cx: &mut Context<Self>) -> Stateful<Div> {
        let tag = self.child_id("close");
        div()
            .id(tag.clone())
            .debug_selector(move || tag.to_string())
            .flex()
            .items_center()
            .justify_center()
            .w(BUTTON_SIZE)
            .h(BUTTON_SIZE)
            .rounded(px(BUTTON_RADIUS))
            .text_size(px(BUTTON_TEXT_SIZE))
            .text_color(rgb(colors.text_tertiary))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .child(SharedString::from("\u{2715}"))
            .on_click(cx.listener(|_bar, _event: &ClickEvent, _window, cx| {
                cx.emit(QuickFindBarEvent::DismissRequested);
            }))
    }
}

impl Render for QuickFindBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let counter = div()
            .font_family(&cx.theme().fonts.data)
            .text_size(px(COUNTER_TEXT_SIZE))
            .text_color(rgb(if self.total == 0 {
                colors.status_error
            } else {
                colors.text_tertiary
            }))
            .child(format!("{}/{}", self.current, self.total));

        div()
            .id(self.id.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(BAR_GAP)
            .h(QUICK_FIND_BAR_HEIGHT)
            .px(BAR_PADDING_X)
            .rounded(px(BAR_RADIUS))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.bg_raised))
            .shadow_lg()
            .child(div().w(INPUT_WIDTH).child(self.input.clone()))
            .child(counter)
            .child(self.step_button(false, &colors, cx))
            .child(self.step_button(true, &colors, cx))
            .child(self.case_toggle(&colors, cx))
            .child(self.close_button(&colors, cx))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        Context, Entity, Modifiers, Render, TestAppContext, VisualTestContext, Window, prelude::*,
    };

    use super::{QuickFindBar, QuickFindBarEvent};
    use crate::text_field::{TextFieldEvent, TextFieldState};

    /// Renders one bar as its window's root and records every event it
    /// emits.
    struct BarHost {
        bar: Entity<QuickFindBar>,
    }

    impl Render for BarHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            gpui::div().size_full().child(self.bar.clone())
        }
    }

    fn build_bar_host(
        cx: &mut TestAppContext,
    ) -> (
        Entity<BarHost>,
        &mut VisualTestContext,
        Rc<RefCell<Vec<QuickFindBarEvent>>>,
    ) {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let (host, vcx) = cx.add_window_view(|_window, cx| {
            let bar = cx.new(|cx| QuickFindBar::new("test-find", "Find...", cx));
            cx.subscribe(&bar, move |_host: &mut BarHost, _bar, event, _cx| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
            BarHost { bar }
        });
        (host, vcx, events)
    }

    fn input(host: &Entity<BarHost>, vcx: &mut VisualTestContext) -> Entity<TextFieldState> {
        host.read_with(vcx, |host, cx| host.bar.read(cx).input().clone())
    }

    #[gpui::test]
    fn typing_emits_query_changed_only_when_the_text_actually_changes(cx: &mut TestAppContext) {
        let (host, vcx, events) = build_bar_host(cx);
        let input = input(&host, vcx);

        input.update(vcx, |field, cx| field.set_value("re", cx));
        vcx.run_until_parked();
        input.update(vcx, |field, cx| field.set_value("re", cx));
        vcx.run_until_parked();
        input.update(vcx, |field, cx| field.set_value("ref", cx));
        vcx.run_until_parked();

        assert_eq!(
            *events.borrow(),
            vec![
                QuickFindBarEvent::QueryChanged("re".to_owned()),
                QuickFindBarEvent::QueryChanged("ref".to_owned()),
            ]
        );
    }

    #[gpui::test]
    fn submitting_the_input_requests_a_forward_step(cx: &mut TestAppContext) {
        let (host, vcx, events) = build_bar_host(cx);
        let input = input(&host, vcx);

        input.update(vcx, |_field, cx| cx.emit(TextFieldEvent::Submit));
        vcx.run_until_parked();

        assert_eq!(
            *events.borrow(),
            vec![QuickFindBarEvent::StepRequested { forward: true }]
        );
    }

    #[gpui::test]
    fn the_arrow_buttons_request_steps_in_their_own_directions(cx: &mut TestAppContext) {
        let (_host, vcx, events) = build_bar_host(cx);
        vcx.run_until_parked();

        for (tag, forward) in [("test-find-prev", false), ("test-find-next", true)] {
            let bounds = vcx
                .debug_bounds(tag)
                .expect("the step button must be tagged and painted");
            vcx.simulate_click(bounds.center(), Modifiers::default());
            vcx.run_until_parked();
            assert_eq!(
                events.borrow().last(),
                Some(&QuickFindBarEvent::StepRequested { forward }),
                "clicking {tag} must request that direction"
            );
        }
    }

    #[gpui::test]
    fn the_case_and_close_buttons_request_their_own_actions(cx: &mut TestAppContext) {
        let (_host, vcx, events) = build_bar_host(cx);
        vcx.run_until_parked();

        for (tag, expected) in [
            ("test-find-case", QuickFindBarEvent::CaseToggleRequested),
            ("test-find-close", QuickFindBarEvent::DismissRequested),
        ] {
            let bounds = vcx
                .debug_bounds(tag)
                .expect("the button must be tagged and painted");
            vcx.simulate_click(bounds.center(), Modifiers::default());
            vcx.run_until_parked();
            assert_eq!(
                events.borrow().last(),
                Some(&expected),
                "clicking {tag} must emit exactly its own request"
            );
        }
    }
}
