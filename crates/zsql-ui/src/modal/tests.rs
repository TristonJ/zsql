use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Div, FocusHandle, Modifiers, MouseButton, Render, TestAppContext, Window, div, point,
    prelude::*, px,
};

use super::{Modal, ModalSize};

/// Renders a `Modal` tracking `focus_handle`, plus a background element
/// positioned behind it, so tests can drive escape/close-icon/backdrop
/// interactions and confirm the modal's scrim blocks clicks and raw mouse
/// events from reaching whatever sits behind it.
struct ModalHost {
    focus_handle: FocusHandle,
    closes: Rc<RefCell<u32>>,
    body_clicks: Rc<RefCell<u32>>,
    background_clicks: Rc<RefCell<u32>>,
    background_mouse_downs: Rc<RefCell<u32>>,
}

impl Render for ModalHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let closes = self.closes.clone();
        let body_clicks = self.body_clicks.clone();
        let background_clicks = self.background_clicks.clone();
        let background_mouse_downs = self.background_mouse_downs.clone();

        let background = div()
            .id("modal-host-background")
            .debug_selector(|| "modal-host-background".to_owned())
            .absolute()
            .inset_0()
            .on_click(move |_event, _window, _cx| {
                *background_clicks.borrow_mut() += 1;
            })
            .on_mouse_down(MouseButton::Left, move |_event, _window, _cx| {
                *background_mouse_downs.borrow_mut() += 1;
            });

        let modal = Modal::<Div, Div>::new("test-modal")
            .track_focus(&self.focus_handle)
            .on_close(move |(), _window, _cx| {
                *closes.borrow_mut() += 1;
            })
            .head(div().child("Head"))
            .body(
                div()
                    .id("modal-host-body-probe")
                    .debug_selector(|| "modal-host-body-probe".to_owned())
                    .w(px(200.0))
                    .h(px(100.0))
                    .on_click(move |_event, _window, _cx| {
                        *body_clicks.borrow_mut() += 1;
                    })
                    .child("Body"),
            );

        div().size_full().child(background).child(modal)
    }
}

/// The counters a [`ModalHost`] test fixture reports back to its caller.
struct ModalHostProbes {
    closes: Rc<RefCell<u32>>,
    body_clicks: Rc<RefCell<u32>>,
    background_clicks: Rc<RefCell<u32>>,
    background_mouse_downs: Rc<RefCell<u32>>,
}

fn build_modal_host(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<ModalHost>,
    &mut gpui::VisualTestContext,
    ModalHostProbes,
) {
    let closes = Rc::new(RefCell::new(0));
    let body_clicks = Rc::new(RefCell::new(0));
    let background_clicks = Rc::new(RefCell::new(0));
    let background_mouse_downs = Rc::new(RefCell::new(0));
    let closes_for_host = closes.clone();
    let body_clicks_for_host = body_clicks.clone();
    let background_clicks_for_host = background_clicks.clone();
    let background_mouse_downs_for_host = background_mouse_downs.clone();
    let (host, vcx) = cx.add_window_view(|_window, cx| ModalHost {
        focus_handle: cx.focus_handle(),
        closes: closes_for_host,
        body_clicks: body_clicks_for_host,
        background_clicks: background_clicks_for_host,
        background_mouse_downs: background_mouse_downs_for_host,
    });
    (
        host,
        vcx,
        ModalHostProbes {
            closes,
            body_clicks,
            background_clicks,
            background_mouse_downs,
        },
    )
}

#[gpui::test]
fn pressing_escape_while_the_modal_is_focused_invokes_on_close_exactly_once(
    cx: &mut TestAppContext,
) {
    let (host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    let focus_handle = host.read_with(vcx, |host, _app| host.focus_handle.clone());
    vcx.update(|window, _cx| window.focus(&focus_handle));
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    assert_eq!(*probes.closes.borrow(), 1);
}

#[gpui::test]
fn escape_does_nothing_when_the_modal_is_not_focused(cx: &mut TestAppContext) {
    let (_host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    assert_eq!(*probes.closes.borrow(), 0);
}

#[gpui::test]
fn clicking_the_close_icon_invokes_on_close(cx: &mut TestAppContext) {
    let (_host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("test-modal-close-icon")
        .expect("the close icon must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*probes.closes.borrow(), 1);
}

#[gpui::test]
fn clicking_inside_the_panel_body_does_not_invoke_on_close(cx: &mut TestAppContext) {
    let (_host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("modal-host-body-probe")
        .expect("the body probe must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *probes.body_clicks.borrow(),
        1,
        "the click must actually reach the body"
    );
    assert_eq!(
        *probes.closes.borrow(),
        0,
        "a click that lands inside the panel must not close the modal"
    );
}

#[gpui::test]
fn clicking_the_backdrop_does_not_dismiss_the_modal(cx: &mut TestAppContext) {
    let (_host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    let panel = vcx
        .debug_bounds("test-modal-panel")
        .expect("the panel must be tagged and painted");
    // A point on the scrim, well outside the panel: this pins the modal's
    // current "no click-out" behavior rather than assuming dismiss-on-click.
    let outside_panel = point(panel.origin.x - px(20.0), panel.origin.y - px(20.0));
    vcx.simulate_click(outside_panel, Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*probes.closes.borrow(), 0);
}

#[gpui::test]
fn clicking_over_an_open_modal_is_swallowed_before_reaching_elements_behind_it(
    cx: &mut TestAppContext,
) {
    let (_host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    let panel = vcx
        .debug_bounds("test-modal-panel")
        .expect("the panel must be tagged and painted");
    vcx.simulate_click(panel.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *probes.background_clicks.borrow(),
        0,
        "a click landing on the modal must not also register on the background \
         (the panel and scrim both stop click propagation)"
    );
}

#[gpui::test]
fn a_mouse_down_over_the_scrim_does_not_reach_the_occluded_background(cx: &mut TestAppContext) {
    let (_host, vcx, probes) = build_modal_host(cx);
    vcx.run_until_parked();

    let panel = vcx
        .debug_bounds("test-modal-panel")
        .expect("the panel must be tagged and painted");
    // A raw mouse-down (not a full click) on the scrim, outside the panel.
    // Nothing on the scrim handles mouse-down, so this only stays off the
    // background if the scrim's `.occlude()` blocks the hit-test itself.
    let outside_panel = point(panel.origin.x - px(20.0), panel.origin.y - px(20.0));
    vcx.simulate_mouse_down(outside_panel, MouseButton::Left, Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *probes.background_mouse_downs.borrow(),
        0,
        "the scrim's occlude() must block the background's hitbox while the modal is open"
    );
}

/// Renders a `Modal` built with `has_close_icon(false)`, isolated from
/// [`ModalHost`] since this fixture has no callbacks to probe.
struct NoCloseIconModalHost;

impl Render for NoCloseIconModalHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            Modal::<Div, Div>::new("test-modal")
                .has_close_icon(false)
                .head(div().child("Head"))
                .body(div().child("Body")),
        )
    }
}

#[gpui::test]
fn a_modal_built_without_a_close_icon_omits_it(cx: &mut TestAppContext) {
    let (_host, vcx) = cx.add_window_view(|_window, _cx| NoCloseIconModalHost);
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("test-modal-close-icon").is_none(),
        "has_close_icon(false) must omit the close icon element entirely"
    );
}

#[gpui::test]
fn modal_size_small_and_wide_return_their_documented_pixel_values(_cx: &mut TestAppContext) {
    assert_eq!(ModalSize::Small.width(), px(468.0));
    assert_eq!(ModalSize::Wide.width(), px(760.0));
    assert_eq!(ModalSize::Small.radius(), px(10.0));
    assert_eq!(ModalSize::Wide.radius(), px(10.0));
    assert_eq!(ModalSize::Small.head_height(), px(44.0));
    assert_eq!(ModalSize::Wide.head_height(), px(44.0));
}
