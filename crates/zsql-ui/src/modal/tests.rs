use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Div, FocusHandle, Modifiers, Render, TestAppContext, Window, div, point, prelude::*,
    px,
};

use super::{Modal, ModalSize};

/// Renders a `Modal` tracking `focus_handle`, plus a background element
/// positioned behind it, so tests can drive escape/close-icon/backdrop
/// interactions and confirm the modal's scrim blocks clicks from reaching
/// whatever sits behind it.
struct ModalHost {
    focus_handle: FocusHandle,
    closes: Rc<RefCell<u32>>,
    body_clicks: Rc<RefCell<u32>>,
    background_clicks: Rc<RefCell<u32>>,
}

impl Render for ModalHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let closes = self.closes.clone();
        let body_clicks = self.body_clicks.clone();
        let background_clicks = self.background_clicks.clone();

        let background = div()
            .id("modal-host-background")
            .debug_selector(|| "modal-host-background".to_owned())
            .absolute()
            .inset_0()
            .on_click(move |_event, _window, _cx| {
                *background_clicks.borrow_mut() += 1;
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

#[expect(
    clippy::type_complexity,
    reason = "a small, self-contained test fixture tuple; a named struct would only push the same fields one level down"
)]
fn build_modal_host(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<ModalHost>,
    &mut gpui::VisualTestContext,
    Rc<RefCell<u32>>,
    Rc<RefCell<u32>>,
    Rc<RefCell<u32>>,
) {
    let closes = Rc::new(RefCell::new(0));
    let body_clicks = Rc::new(RefCell::new(0));
    let background_clicks = Rc::new(RefCell::new(0));
    let closes_for_host = closes.clone();
    let body_clicks_for_host = body_clicks.clone();
    let background_clicks_for_host = background_clicks.clone();
    let (host, vcx) = cx.add_window_view(|_window, cx| ModalHost {
        focus_handle: cx.focus_handle(),
        closes: closes_for_host,
        body_clicks: body_clicks_for_host,
        background_clicks: background_clicks_for_host,
    });
    (host, vcx, closes, body_clicks, background_clicks)
}

#[gpui::test]
fn pressing_escape_while_the_modal_is_focused_invokes_on_close_exactly_once(
    cx: &mut TestAppContext,
) {
    let (host, vcx, closes, _body_clicks, _background_clicks) = build_modal_host(cx);
    vcx.run_until_parked();

    let focus_handle = host.read_with(vcx, |host, _app| host.focus_handle.clone());
    vcx.update(|window, _cx| window.focus(&focus_handle));
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    assert_eq!(*closes.borrow(), 1);
}

#[gpui::test]
fn escape_does_nothing_when_the_modal_is_not_focused(cx: &mut TestAppContext) {
    let (_host, vcx, closes, _body_clicks, _background_clicks) = build_modal_host(cx);
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    assert_eq!(*closes.borrow(), 0);
}

#[gpui::test]
fn clicking_the_close_icon_invokes_on_close(cx: &mut TestAppContext) {
    let (_host, vcx, closes, _body_clicks, _background_clicks) = build_modal_host(cx);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("test-modal-close-icon")
        .expect("the close icon must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*closes.borrow(), 1);
}

#[gpui::test]
fn clicking_inside_the_panel_body_does_not_invoke_on_close(cx: &mut TestAppContext) {
    let (_host, vcx, closes, body_clicks, _background_clicks) = build_modal_host(cx);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("modal-host-body-probe")
        .expect("the body probe must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *body_clicks.borrow(),
        1,
        "the click must actually reach the body"
    );
    assert_eq!(
        *closes.borrow(),
        0,
        "a click that lands inside the panel must not close the modal"
    );
}

#[gpui::test]
fn clicking_the_backdrop_does_not_dismiss_the_modal(cx: &mut TestAppContext) {
    let (_host, vcx, closes, _body_clicks, _background_clicks) = build_modal_host(cx);
    vcx.run_until_parked();

    let panel = vcx
        .debug_bounds("test-modal-panel")
        .expect("the panel must be tagged and painted");
    // A point on the scrim, well outside the panel: this pins the modal's
    // current "no click-out" behavior rather than assuming dismiss-on-click.
    let outside_panel = point(panel.origin.x - px(20.0), panel.origin.y - px(20.0));
    vcx.simulate_click(outside_panel, Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*closes.borrow(), 0);
}

#[gpui::test]
fn clicking_over_an_open_modal_does_not_reach_elements_behind_it(cx: &mut TestAppContext) {
    let (_host, vcx, _closes, _body_clicks, background_clicks) = build_modal_host(cx);
    vcx.run_until_parked();

    let panel = vcx
        .debug_bounds("test-modal-panel")
        .expect("the panel must be tagged and painted");
    vcx.simulate_click(panel.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *background_clicks.borrow(),
        0,
        "the modal's scrim must occlude clicks from reaching elements behind it"
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
