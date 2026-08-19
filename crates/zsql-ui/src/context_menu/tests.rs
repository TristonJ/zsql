use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Entity, MouseButton, Pixels, Point, Render, TestAppContext, VisualTestContext, Window,
    point, prelude::*, px,
};

use super::{ContextMenu, ContextMenuItem};

/// Renders a fixed context menu ("Item A", an optional separator, "Item B",
/// an item with no `on_click` handler at all, and a disabled item that
/// carries an `on_click` handler it must never invoke) anchored at a
/// caller-chosen point, recording which item was clicked and how many times
/// `on_close` fired.
struct MenuHost {
    position: Point<Pixels>,
    include_separator: bool,
    clicks: Rc<RefCell<Vec<&'static str>>>,
    closes: Rc<RefCell<u32>>,
}

impl Render for MenuHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let clicks_a = self.clicks.clone();
        let clicks_b = self.clicks.clone();
        let clicks_disabled = self.clicks.clone();
        let clicks_danger = self.clicks.clone();
        let closes = self.closes.clone();
        let mut menu = ContextMenu::new("menu-host")
            .position(self.position)
            .on_close(move |(), _window, _cx| {
                *closes.borrow_mut() += 1;
            })
            .add_item(
                ContextMenuItem::new("Item A").on_click(move |_event, _window, _cx| {
                    clicks_a.borrow_mut().push("Item A");
                }),
            );
        if self.include_separator {
            menu = menu.add_separator();
        }
        let menu = menu
            .add_item(
                ContextMenuItem::new("Item B").on_click(move |_event, _window, _cx| {
                    clicks_b.borrow_mut().push("Item B");
                }),
            )
            .add_item(ContextMenuItem::new("No Handler"))
            .add_item(
                ContextMenuItem::new("Disabled Item")
                    .on_click(move |_event, _window, _cx| {
                        clicks_disabled.borrow_mut().push("Disabled Item");
                    })
                    .disabled(true),
            )
            .add_item(
                ContextMenuItem::new("Hinted Item")
                    .disabled(true)
                    .hint("needs a primary key"),
            )
            .add_item(ContextMenuItem::new("Danger Item").danger(true).on_click(
                move |_event, _window, _cx| {
                    clicks_danger.borrow_mut().push("Danger Item");
                },
            ));
        // A menu rendered as a bare window root (rather than nested inside a
        // larger view, as it always is in production) needs an explicit
        // full-size container so its `deferred`/`absolute` backdrop resolves
        // against the whole window instead of a zero-size root.
        gpui::div().size_full().child(menu)
    }
}

#[expect(
    clippy::type_complexity,
    reason = "a small, self-contained test fixture tuple; a named struct would only push the same fields one level down"
)]
fn build_menu_host(
    cx: &mut TestAppContext,
    position: Point<Pixels>,
    include_separator: bool,
) -> (
    Entity<MenuHost>,
    &mut VisualTestContext,
    Rc<RefCell<Vec<&'static str>>>,
    Rc<RefCell<u32>>,
) {
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let closes = Rc::new(RefCell::new(0));
    let clicks_for_host = clicks.clone();
    let closes_for_host = closes.clone();
    let (host, vcx) = cx.add_window_view(|_window, _cx| MenuHost {
        position,
        include_separator,
        clicks: clicks_for_host,
        closes: closes_for_host,
    });
    (host, vcx, clicks, closes)
}

fn anchor() -> Point<Pixels> {
    point(px(60.0), px(60.0))
}

#[gpui::test]
fn an_explicit_position_anchors_the_menu_at_that_point(cx: &mut TestAppContext) {
    let (_host, vcx, _clicks, _closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("menu-host")
        .expect("the menu must be tagged and painted");
    assert_eq!(bounds.origin, anchor());
}

#[gpui::test]
fn clicking_an_item_invokes_only_that_items_on_click(cx: &mut TestAppContext) {
    let (_host, vcx, clicks, closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("Item B")
        .expect("item b must be tagged and painted");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*clicks.borrow(), vec!["Item B"]);
    assert_eq!(
        *closes.borrow(),
        0,
        "clicking an item inside the menu must not also trigger on_close"
    );
}

#[gpui::test]
fn an_item_with_no_on_click_renders_and_can_be_clicked_without_panicking(cx: &mut TestAppContext) {
    let (_host, vcx, clicks, _closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("No Handler")
        .expect("the handlerless item must still be tagged and painted");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(
        clicks.borrow().is_empty(),
        "a handlerless item must not record any click"
    );
}

#[gpui::test]
fn a_disabled_items_on_click_never_fires_even_though_it_still_renders(cx: &mut TestAppContext) {
    let (_host, vcx, clicks, closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("Disabled Item")
        .expect("a disabled item must still be tagged and painted, just dimmed");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(
        clicks.borrow().is_empty(),
        "a disabled item's on_click must never fire, even though one was attached"
    );
    assert_eq!(
        *closes.borrow(),
        0,
        "clicking a disabled item must not fall through to closing the menu either"
    );
}

#[gpui::test]
fn left_mouse_down_outside_the_menu_invokes_on_close(cx: &mut TestAppContext) {
    let (_host, vcx, _clicks, closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    vcx.simulate_mouse_down(
        point(px(1500.0), px(900.0)),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    assert_eq!(*closes.borrow(), 1);
}

#[gpui::test]
fn right_mouse_down_outside_the_menu_invokes_on_close(cx: &mut TestAppContext) {
    let (_host, vcx, _clicks, closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    vcx.simulate_mouse_down(
        point(px(1500.0), px(900.0)),
        MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    assert_eq!(*closes.borrow(), 1);
}

#[gpui::test]
fn a_hint_renders_alongside_a_disabled_items_label(cx: &mut TestAppContext) {
    let (_host, vcx, _clicks, _closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("Hinted Item").is_some(),
        "an item carrying a hint must still render and be tagged"
    );
    assert!(
        vcx.debug_bounds("Hinted Item-hint").is_some(),
        "the hint text itself must render as its own tagged element"
    );
}

#[gpui::test]
fn a_danger_item_renders_and_its_click_handler_fires(cx: &mut TestAppContext) {
    let (_host, vcx, clicks, _closes) = build_menu_host(cx, anchor(), false);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("Danger Item")
        .expect("a danger-styled item must still render and be tagged");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*clicks.borrow(), vec!["Danger Item"]);
}

#[gpui::test]
fn add_separator_inserts_a_separator_between_the_items_added_before_and_after_it(
    cx: &mut TestAppContext,
) {
    let (_host, vcx, _clicks, _closes) = build_menu_host(cx, anchor(), true);
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("menu-host-separator-0").is_none(),
        "no separator was added before item 0 (Item A)"
    );
    let separator = vcx
        .debug_bounds("menu-host-separator-1")
        .expect("a separator was added after item 0 (Item A), before item 1 (Item B)");
    let item_a = vcx
        .debug_bounds("Item A")
        .expect("item a must be tagged and painted");
    let item_b = vcx
        .debug_bounds("Item B")
        .expect("item b must be tagged and painted");

    assert!(
        separator.origin.y >= item_a.bottom(),
        "the separator must sit at or below item a's bottom edge"
    );
    assert!(
        separator.bottom() <= item_b.origin.y,
        "the separator must sit at or above item b's top edge"
    );
}
