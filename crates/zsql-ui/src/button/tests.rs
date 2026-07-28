use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Context, Entity, Modifiers, MouseButton, Render, TestAppContext, Window, prelude::*};

use super::{ButtonSwitch, button_base};

/// Renders two independently-`id`ed buttons built on [`button_base`] side by
/// side, tagging each with a debug selector so a test can locate its painted
/// bounds. Grabs the same per-instance hover entity `button_base` hands to
/// its callers (`primary_button`/`secondary_button`/`destructive_button` all
/// delegate to it) so hover state can be asserted without reaching into any
/// private field.
struct TwoButtonsHost {
    hover_a: Option<Entity<bool>>,
    hover_b: Option<Entity<bool>>,
}

impl Render for TwoButtonsHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (a, hover_a) = button_base("hover-host-a", window, cx);
        let (b, hover_b) = button_base("hover-host-b", window, cx);
        self.hover_a = Some(hover_a);
        self.hover_b = Some(hover_b);
        gpui::div()
            .child(a.debug_selector(|| "hover-host-a".to_owned()))
            .child(b.debug_selector(|| "hover-host-b".to_owned()))
    }
}

#[gpui::test]
fn two_buttons_built_with_different_ids_own_distinct_hover_entities(cx: &mut TestAppContext) {
    let (host, vcx) = cx.add_window_view(|_window, _cx| TwoButtonsHost {
        hover_a: None,
        hover_b: None,
    });
    vcx.run_until_parked();

    let (hover_a, hover_b) = host.read_with(vcx, |host, _app| {
        (
            host.hover_a.clone().expect("populated on first render"),
            host.hover_b.clone().expect("populated on first render"),
        )
    });
    assert_ne!(
        hover_a.entity_id(),
        hover_b.entity_id(),
        "each button must own its own hover entity, not alias a shared one"
    );
    assert!(!hover_a.read_with(vcx, |hovered, _app| *hovered));
    assert!(!hover_b.read_with(vcx, |hovered, _app| *hovered));
}

#[gpui::test]
fn hovering_one_button_does_not_flip_a_second_buttons_hover_state(cx: &mut TestAppContext) {
    let (host, vcx) = cx.add_window_view(|_window, _cx| TwoButtonsHost {
        hover_a: None,
        hover_b: None,
    });
    vcx.run_until_parked();

    let (hover_a, hover_b) = host.read_with(vcx, |host, _app| {
        (
            host.hover_a.clone().expect("populated on first render"),
            host.hover_b.clone().expect("populated on first render"),
        )
    });

    let bounds_a = vcx
        .debug_bounds("hover-host-a")
        .expect("button a must be tagged and painted");
    vcx.simulate_mouse_move(bounds_a.center(), None::<MouseButton>, Modifiers::default());
    vcx.run_until_parked();

    assert!(
        hover_a.read_with(vcx, |hovered, _app| *hovered),
        "the hovered button must flip its own hover state"
    );
    assert!(
        !hover_b.read_with(vcx, |hovered, _app| *hovered),
        "hovering one button must not affect a second button's hover state"
    );
}

/// A smoke-test host for the three named button variants, confirming they
/// each build atop the shared hover machinery ([`button_base`]) without
/// panicking, matching the hover behavior exercised above.
struct NamedVariantsHost;

impl Render for NamedVariantsHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .child(super::primary_button("variant-primary", window, cx).child("Primary"))
            .child(super::secondary_button("variant-secondary", window, cx).child("Secondary"))
            .child(super::destructive_button("variant-destructive", window, cx).child("Destroy"))
    }
}

#[gpui::test]
fn primary_secondary_and_destructive_buttons_render_without_panicking(cx: &mut TestAppContext) {
    let (_host, vcx) = cx.add_window_view(|_window, _cx| NamedVariantsHost);
    vcx.run_until_parked();
}

/// A host rendering a two-option [`ButtonSwitch`], recording which option
/// was clicked so tests can assert click routing without reaching into the
/// switch's private option list.
struct SwitchHost {
    selected: Option<gpui::ElementId>,
    disabled: bool,
    clicks: Rc<RefCell<Vec<&'static str>>>,
}

impl Render for SwitchHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clicks_a = self.clicks.clone();
        let clicks_b = self.clicks.clone();
        let mut switch = ButtonSwitch::new().disabled(self.disabled);
        if let Some(selected) = self.selected.clone() {
            switch = switch.selected(selected);
        }
        switch
            .add_option(window, cx, "switch-a", "A", move |_event, _window, _cx| {
                clicks_a.borrow_mut().push("a");
            })
            .add_option(window, cx, "switch-b", "B", move |_event, _window, _cx| {
                clicks_b.borrow_mut().push("b");
            })
    }
}

fn build_switch_host<'a>(
    cx: &'a mut TestAppContext,
    selected: Option<&'static str>,
    disabled: bool,
) -> (
    Entity<SwitchHost>,
    &'a mut gpui::VisualTestContext,
    Rc<RefCell<Vec<&'static str>>>,
) {
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let clicks_for_host = clicks.clone();
    let selected = selected.map(gpui::ElementId::from);
    let (host, vcx) = cx.add_window_view(|_window, _cx| SwitchHost {
        selected,
        disabled,
        clicks: clicks_for_host,
    });
    (host, vcx, clicks)
}

#[gpui::test]
fn selecting_an_option_tags_it_distinctly_from_the_unselected_option(cx: &mut TestAppContext) {
    let (_host, vcx, _clicks) = build_switch_host(cx, Some("switch-a"), false);
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("switch-a-selected").is_some(),
        "the selected option must paint under its selected-tagged selector"
    );
    assert!(
        vcx.debug_bounds("switch-a").is_none(),
        "a selected option must not also paint under its plain, unselected selector"
    );
    assert!(
        vcx.debug_bounds("switch-b").is_some(),
        "the unselected option must paint under its plain selector"
    );
    assert!(
        vcx.debug_bounds("switch-b-selected").is_none(),
        "an unselected option must not paint under the selected-tagged selector"
    );
}

#[gpui::test]
fn clicking_an_option_on_an_enabled_switch_invokes_its_on_click(cx: &mut TestAppContext) {
    let (_host, vcx, clicks) = build_switch_host(cx, None, false);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("switch-b")
        .expect("option b must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*clicks.borrow(), vec!["b"]);
}

#[gpui::test]
fn clicking_an_option_on_a_disabled_switch_does_not_invoke_its_on_click(cx: &mut TestAppContext) {
    let (_host, vcx, clicks) = build_switch_host(cx, None, true);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("switch-b")
        .expect("option b must still paint while disabled");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert!(
        clicks.borrow().is_empty(),
        "a disabled switch must not invoke any option's on_click"
    );
}
