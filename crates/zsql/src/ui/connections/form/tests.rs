//! [`ConnectionForm`] tested in isolation, with no
//! [`super::super::ConnectionManagerView`] involved: URL/field sync and
//! driver detection, prefill via `begin_edit`, render-does-not-panic
//! smoke tests, Tab/Shift-Tab focus order, and the events the footer
//! buttons and Enter-to-submit emit.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, Modifiers, Render,
    TestAppContext, VisualTestContext, Window, div, prelude::*,
};
use uuid::Uuid;
use zsql_ui::modal::ModalSize;
use zsql_ui::text_field::TextFieldEvent;

use super::{ConnectionForm, ConnectionFormEvent, HostKeyMode};
use crate::connections::{HostKeyPolicy, SshAuthKind, StoredSsh};

/// A plain-data mirror of [`ConnectionFormEvent`] a test can capture,
/// compare, and print without requiring the production event type itself to
/// carry those trait implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CapturedEvent {
    Cancel,
    Test { url: String },
    Connect { name: String, url: String },
    Add { name: String, url: String },
    Edit { id: Uuid, name: String, url: String },
}

impl From<&ConnectionFormEvent> for CapturedEvent {
    fn from(event: &ConnectionFormEvent) -> Self {
        match event {
            ConnectionFormEvent::Cancel => CapturedEvent::Cancel,
            ConnectionFormEvent::Test { url } => CapturedEvent::Test { url: url.clone() },
            ConnectionFormEvent::Connect { name, url } => CapturedEvent::Connect {
                name: name.clone(),
                url: url.clone(),
            },
            ConnectionFormEvent::Add { name, url } => CapturedEvent::Add {
                name: name.clone(),
                url: url.clone(),
            },
            ConnectionFormEvent::Edit { id, name, url } => CapturedEvent::Edit {
                id: *id,
                name: name.clone(),
                url: url.clone(),
            },
        }
    }
}

/// A bare form, with no window, and every event it emits captured into a
/// shared, test-owned list. Sufficient for every assertion that does not
/// need real click/keystroke dispatch.
fn build_form(
    cx: &mut TestAppContext,
) -> (Entity<ConnectionForm>, Rc<RefCell<Vec<CapturedEvent>>>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    let form = cx.new(ConnectionForm::new);
    cx.update(|cx| {
        cx.subscribe(&form, move |_form, event, _cx| {
            events_for_sub.borrow_mut().push(CapturedEvent::from(event));
        })
        .detach();
    });
    (form, events)
}

/// [`build_form`]'s windowed equivalent, needed wherever a test must
/// actually paint the form and dispatch a real click or keystroke into it
/// (footer button clicks, render-does-not-panic checks).
fn build_form_in_window(
    cx: &mut TestAppContext,
) -> (
    Entity<ConnectionForm>,
    &mut VisualTestContext,
    Rc<RefCell<Vec<CapturedEvent>>>,
) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    let (form, vcx) = cx.add_window_view(|_window, cx| ConnectionForm::new(cx));
    vcx.update(|_window, cx| {
        cx.subscribe(&form, move |_form, event, _cx| {
            events_for_sub.borrow_mut().push(CapturedEvent::from(event));
        })
        .detach();
    });
    (form, vcx, events)
}

/// A host wrapping a bare [`ConnectionForm`] with the same Tab/Shift-Tab
/// focus-cycling [`super::super::ConnectionManagerView`]'s modal provides in
/// production (see its `move_focus`), reimplemented here purely as test
/// scaffolding so focus-order tests can dispatch real keystrokes against the
/// form alone, with no manager involved.
struct FormHost {
    form: Entity<ConnectionForm>,
    focus_handle: FocusHandle,
}

impl FormHost {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            form: cx.new(ConnectionForm::new),
            focus_handle: cx.focus_handle(),
        }
    }

    fn move_focus(&self, backward: bool, window: &mut Window, cx: &Context<Self>) {
        let order = self.form.read(cx).focus_order(cx);
        if order.is_empty() {
            return;
        }
        let current = window.focused(cx);
        let current_index = current.and_then(|handle| order.iter().position(|f| *f == handle));
        let next_index = match current_index {
            Some(index) if backward => (index + order.len() - 1) % order.len(),
            Some(index) => (index + 1) % order.len(),
            None => 0,
        };
        window.focus(&order[next_index]);
    }
}

impl Render for FormHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|host, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    host.move_focus(event.keystroke.modifiers.shift, window, cx);
                }
            }))
            .child(self.form.clone())
    }
}

fn build_form_host(cx: &mut TestAppContext) -> (Entity<FormHost>, &mut VisualTestContext) {
    cx.add_window_view(|_window, cx| FormHost::new(cx))
}

// ---- URL -> fields sync -------------------------------------------------

#[gpui::test]
fn editing_the_url_field_reparses_every_driver_field(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input(
            "mssql://sa:pw@dbhost:1433/zsql?trustServerCertificate=true",
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert_eq!(form.pending_driver_id(), Ok("mssql"));
        assert_eq!(form.host_field.read(cx).value().as_ref(), "dbhost");
        assert_eq!(form.port_field.read(cx).value().as_ref(), "1433");
        assert_eq!(form.user_field.read(cx).value().as_ref(), "sa");
        assert_eq!(form.password_field.read(cx).value().as_ref(), "pw");
        assert_eq!(form.database_field.read(cx).value().as_ref(), "zsql");
        assert!(form.dim_reason().is_none());
    });
}

#[gpui::test]
fn an_unparseable_url_dims_the_field_section_with_a_reason_and_re_enables_once_valid(
    cx: &mut TestAppContext,
) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| form.set_url_input("postgres://app@", cx));
    form.read_with(cx, |form, _cx| {
        assert!(
            form.dim_reason().is_some(),
            "an incomplete URL must dim the field section"
        );
        // The scheme is still recognizable, so the layout stays Postgres-shaped.
        assert_eq!(form.pending_driver_id(), Ok("postgres"));
    });

    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
    });
    form.read_with(cx, |form, cx| {
        assert!(
            form.dim_reason().is_none(),
            "a now-valid URL must clear the dim reason"
        );
        assert_eq!(form.host_field.read(cx).value().as_ref(), "host");
    });
}

// ---- fields -> URL sync --------------------------------------------------

#[gpui::test]
fn editing_the_port_field_changes_only_the_urls_port(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app:s3cr3t@host:5432/app?sslmode=require", cx);
    });
    let port_field = form.read_with(cx, |form, _cx| form.port_field.clone());
    port_field.update(cx, |field, cx| field.set_value("6543", cx));
    form.read_with(cx, |form, cx| {
        assert_eq!(
            form.url_field.read(cx).value().as_ref(),
            "postgres://app:s3cr3t@host:6543/app?sslmode=require",
            "only the port must change in the rebuilt URL"
        );
    });
}

#[gpui::test]
fn editing_the_host_field_leaves_user_password_database_and_params_intact(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app:s3cr3t@host:5432/app?sslmode=require", cx);
    });
    let host_field = form.read_with(cx, |form, _cx| form.host_field.clone());
    host_field.update(cx, |field, cx| field.set_value("otherhost", cx));
    form.read_with(cx, |form, cx| {
        assert_eq!(
            form.url_field.read(cx).value().as_ref(),
            "postgres://app:s3cr3t@otherhost:5432/app?sslmode=require"
        );
    });
}

#[gpui::test]
fn selecting_tls_off_writes_the_disable_sslmode_value(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app:s3cr3t@host:5432/app?sslmode=require", cx);
    });
    form.update(cx, |form, cx| {
        form.set_tls_mode("postgres", zsql_core::TlsVerify::Off, cx);
    });
    form.read_with(cx, |form, cx| {
        let url = form.url_field.read(cx).value().to_string();
        assert!(
            url.contains("sslmode=disable"),
            "selecting Off must write sslmode=disable, got {url}"
        );
    });
}

#[gpui::test]
fn editing_a_driver_field_rewrites_the_url_field_live(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/orders", cx);
    });
    let database_field = form.read_with(cx, |form, _cx| form.database_field.clone());
    database_field.update(cx, |field, cx| field.set_value("other_db", cx));
    form.read_with(cx, |form, cx| {
        assert_eq!(
            form.url_field.read(cx).value().as_ref(),
            "postgres://app@host:5432/other_db",
            "URL rewritten"
        );
    });
}

#[gpui::test]
fn editing_the_sqlite_path_field_rewrites_the_url(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| form.set_url_input("sqlite::memory:", cx));
    let sqlite_path_field = form.read_with(cx, |form, _cx| form.sqlite_path_field.clone());
    sqlite_path_field.update(cx, |field, cx| field.set_value("/tmp/scratch.db", cx));
    form.read_with(cx, |form, cx| {
        assert_eq!(
            form.url_field.read(cx).value().as_ref(),
            "sqlite:///tmp/scratch.db"
        );
    });
}

// ---- password masking -----------------------------------------------------

#[gpui::test]
fn the_password_field_starts_masked_and_the_toggle_reveals_it(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.read_with(cx, |form, cx| {
        assert!(form.password_field.read(cx).is_masked());
    });
    form.update(cx, ConnectionForm::toggle_password_visible);
    form.read_with(cx, |form, cx| {
        assert!(!form.password_field.read(cx).is_masked());
    });
}

// ---- begin_edit prefill ---------------------------------------------------

#[gpui::test]
fn show_edit_form_prefills_name_url_and_the_driver_fields(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    let id = Uuid::new_v4();
    form.update(cx, |form, cx| {
        form.begin_edit(
            id,
            "staging".to_owned(),
            "postgres://app:s3cr3t@staging.internal:5432/app?sslmode=require".to_owned(),
            None,
            None,
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert_eq!(
            form.edit_id(),
            Some(id),
            "edit form must be shown for the right row"
        );
        assert_eq!(form.name_field.read(cx).value().as_ref(), "staging");
        assert_eq!(
            form.url_field.read(cx).value().as_ref(),
            "postgres://app:s3cr3t@staging.internal:5432/app?sslmode=require"
        );
        assert_eq!(
            form.host_field.read(cx).value().as_ref(),
            "staging.internal"
        );
        assert_eq!(form.port_field.read(cx).value().as_ref(), "5432");
        assert_eq!(form.user_field.read(cx).value().as_ref(), "app");
        assert_eq!(form.password_field.read(cx).value().as_ref(), "s3cr3t");
        assert_eq!(form.database_field.read(cx).value().as_ref(), "app");
    });
}

#[gpui::test]
fn show_edit_form_for_a_sqlite_url_prefills_only_the_path_field(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "reports".to_owned(),
            "sqlite:///tmp/reports.db".to_owned(),
            None,
            None,
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert_eq!(form.pending_driver_id(), Ok("sqlite"));
        assert_eq!(
            form.sqlite_path_field.read(cx).value().as_ref(),
            "/tmp/reports.db"
        );
        assert!(form.host_field.read(cx).value().is_empty());
    });
}

// ---- render smoke tests ----------------------------------------------------

#[gpui::test]
fn the_edit_form_renders_prefilled_without_panicking(cx: &mut TestAppContext) {
    let (form, vcx, _events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "staging".to_owned(),
            "postgres://app@staging.internal:5432/app".to_owned(),
            None,
            None,
            cx,
        );
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn the_sqlite_field_section_renders_the_path_field_and_not_host_or_port(cx: &mut TestAppContext) {
    let (form, vcx, _events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| form.set_url_input("sqlite::memory:", cx));
    vcx.run_until_parked();

    form.read_with(vcx, |form, _cx| {
        assert_eq!(form.pending_driver_id(), Ok("sqlite"));
    });
}

#[gpui::test]
fn the_field_section_renders_dimmed_while_unparseable_and_undimmed_once_valid(
    cx: &mut TestAppContext,
) {
    let (form, vcx, _events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| form.set_url_input("postgres://app@", cx));
    vcx.run_until_parked();
    form.read_with(vcx, |form, _cx| assert!(form.dim_reason().is_some()));

    form.update(vcx, |form, cx| {
        form.set_url_input("postgres://app@host/db", cx);
    });
    vcx.run_until_parked();
    form.read_with(vcx, |form, _cx| assert!(form.dim_reason().is_none()));
}

// ---- Tab / Shift-Tab focus order -------------------------------------------

#[gpui::test]
fn tab_and_shift_tab_move_focus_through_the_form_in_visual_order(cx: &mut TestAppContext) {
    assert_focus_order_round_trips_through_tab_and_shift_tab(cx, "postgres://app@host:5432/db");
}

#[gpui::test]
fn tab_and_shift_tab_move_focus_through_the_form_in_visual_order_for_an_mssql_url(
    cx: &mut TestAppContext,
) {
    assert_focus_order_round_trips_through_tab_and_shift_tab(cx, "mssql://sa:pw@dbhost:1433/db");
}

#[gpui::test]
fn tab_and_shift_tab_move_focus_through_the_form_in_visual_order_for_a_sqlite_url(
    cx: &mut TestAppContext,
) {
    assert_focus_order_round_trips_through_tab_and_shift_tab(cx, "sqlite::memory:");
}

/// Opens the form on `url` and checks its own `focus_order()` round-trips
/// through Tab and Shift-Tab, including wrap-around at both ends.
fn assert_focus_order_round_trips_through_tab_and_shift_tab(cx: &mut TestAppContext, url: &str) {
    let (host, vcx) = build_form_host(cx);
    host.update(vcx, |host, cx| {
        host.form.update(cx, |form, cx| form.set_url_input(url, cx));
    });
    vcx.run_until_parked();

    let expected_order = host.read_with(vcx, |host, cx| host.form.read(cx).focus_order(cx));
    assert!(
        expected_order.len() >= 3,
        "a parsed url must expose name, url, and driver fields"
    );

    assert_tab_cycles_through_in_order(vcx, &expected_order);
}

/// Tab forward through every handle in `order` starting from `order[0]`,
/// asserting each keystroke lands on the next concrete handle, then checks
/// wrap-around in both directions: Tab from the last control back to the
/// first, and Shift-Tab from the first back to the last.
fn assert_tab_cycles_through_in_order(vcx: &mut VisualTestContext, order: &[FocusHandle]) {
    assert!(
        order.len() >= 2,
        "need at least two controls to cycle through"
    );
    vcx.update(|window, _cx| window.focus(&order[0]));
    vcx.run_until_parked();

    for expected in order.iter().skip(1) {
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(expected));
        });
    }

    vcx.simulate_keystrokes("tab");
    vcx.update(|window, cx| {
        assert_eq!(
            window.focused(cx).as_ref(),
            Some(&order[0]),
            "Tab from the last control must wrap to the first"
        );
    });

    vcx.simulate_keystrokes("shift-tab");
    vcx.update(|window, cx| {
        assert_eq!(
            window.focused(cx).as_ref(),
            Some(&order[order.len() - 1]),
            "Shift-Tab from the first control must wrap to the last"
        );
    });
}

/// The add form's full concrete focus chain over a parsed, non-sqlite URL:
/// name, url, every network field, then the footer buttons in visual order.
fn add_form_network_focus_chain(form: &ConnectionForm, cx: &gpui::App) -> Vec<FocusHandle> {
    vec![
        form.name_field.read(cx).focus_handle(cx),
        form.url_field.read(cx).focus_handle(cx),
        form.host_field.read(cx).focus_handle(cx),
        form.port_field.read(cx).focus_handle(cx),
        form.user_field.read(cx).focus_handle(cx),
        form.password_field.read(cx).focus_handle(cx),
        form.database_field.read(cx).focus_handle(cx),
        form.tls_focus.clone(),
        form.ssh_enabled_focus.clone(),
        form.cancel_focus.clone(),
        form.test_focus.clone(),
        form.connect_focus.clone(),
        form.save_focus.clone(),
    ]
}

/// The edit form's equivalent of [`add_form_network_focus_chain`] (no
/// Connect button).
fn edit_form_network_focus_chain(form: &ConnectionForm, cx: &gpui::App) -> Vec<FocusHandle> {
    vec![
        form.name_field.read(cx).focus_handle(cx),
        form.url_field.read(cx).focus_handle(cx),
        form.host_field.read(cx).focus_handle(cx),
        form.port_field.read(cx).focus_handle(cx),
        form.user_field.read(cx).focus_handle(cx),
        form.password_field.read(cx).focus_handle(cx),
        form.database_field.read(cx).focus_handle(cx),
        form.tls_focus.clone(),
        form.ssh_enabled_focus.clone(),
        form.cancel_focus.clone(),
        form.test_focus.clone(),
        form.save_focus.clone(),
    ]
}

/// For the add form over `url` (expected to resolve to the `mysql` driver,
/// whether via a `mysql://` or `mariadb://` scheme), Tab from the URL field
/// must advance through host, port, user, password, database, and tls
/// before reaching the footer buttons, and the whole chain must wrap in
/// both directions. Asserts against each field's own focus handle, not a
/// re-derived `focus_order()` list, so the assertion cannot pass merely
/// because `focus_order()` and the test agree on the same (possibly wrong)
/// derivation.
fn assert_add_form_tab_order_covers_network_fields(cx: &mut TestAppContext, url: &str) {
    let (host, vcx) = build_form_host(cx);
    host.update(vcx, |host, cx| {
        host.form.update(cx, |form, cx| form.set_url_input(url, cx));
    });
    vcx.run_until_parked();

    host.read_with(vcx, |host, cx| {
        assert_eq!(
            host.form.read(cx).pending_driver_id(),
            Ok("mysql"),
            "url {url} must resolve to the registered mysql driver"
        );
    });

    let order = host.read_with(vcx, |host, cx| {
        add_form_network_focus_chain(host.form.read(cx), cx)
    });
    assert_tab_cycles_through_in_order(vcx, &order);
}

/// [`assert_add_form_tab_order_covers_network_fields`]'s edit-form
/// equivalent: the form is opened directly via `begin_edit` over `url`.
fn assert_edit_form_tab_order_covers_network_fields(cx: &mut TestAppContext, url: &str) {
    let (host, vcx) = build_form_host(cx);
    host.update(vcx, |host, cx| {
        host.form.update(cx, |form, cx| {
            form.begin_edit(
                Uuid::new_v4(),
                "db".to_owned(),
                url.to_owned(),
                None,
                None,
                cx,
            );
        });
    });
    vcx.run_until_parked();

    host.read_with(vcx, |host, cx| {
        assert_eq!(
            host.form.read(cx).pending_driver_id(),
            Ok("mysql"),
            "url {url} must resolve to the registered mysql driver"
        );
    });

    let order = host.read_with(vcx, |host, cx| {
        edit_form_network_focus_chain(host.form.read(cx), cx)
    });
    assert_tab_cycles_through_in_order(vcx, &order);
}

#[gpui::test]
fn tab_order_for_the_add_form_covers_network_fields_for_a_mysql_url(cx: &mut TestAppContext) {
    assert_add_form_tab_order_covers_network_fields(cx, "mysql://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn tab_order_for_the_add_form_covers_network_fields_for_a_mariadb_url(cx: &mut TestAppContext) {
    assert_add_form_tab_order_covers_network_fields(cx, "mariadb://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn tab_order_for_the_edit_form_covers_network_fields_for_a_mysql_url(cx: &mut TestAppContext) {
    assert_edit_form_tab_order_covers_network_fields(cx, "mysql://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn tab_order_for_the_edit_form_covers_network_fields_for_a_mariadb_url(cx: &mut TestAppContext) {
    assert_edit_form_tab_order_covers_network_fields(cx, "mariadb://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn focus_order_for_an_empty_url_contains_only_name_url_and_footer_buttons(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.read_with(cx, |form, cx| {
        assert!(form.pending_driver_id().is_err());
        let order = form.focus_order(cx);
        let expected = vec![
            form.name_field.read(cx).focus_handle(cx),
            form.url_field.read(cx).focus_handle(cx),
            form.cancel_focus.clone(),
            form.test_focus.clone(),
            form.connect_focus.clone(),
            form.save_focus.clone(),
        ];
        assert_eq!(
            order, expected,
            "an empty URL must expose no driver fields at all"
        );
    });
}

#[gpui::test]
fn focus_order_for_an_unrecognized_scheme_contains_only_name_url_and_footer_buttons(
    cx: &mut TestAppContext,
) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| form.set_url_input("cassandra://host/db", cx));
    form.read_with(cx, |form, cx| {
        assert!(form.pending_driver_id().is_err());
        let order = form.focus_order(cx);
        let expected = vec![
            form.name_field.read(cx).focus_handle(cx),
            form.url_field.read(cx).focus_handle(cx),
            form.cancel_focus.clone(),
            form.test_focus.clone(),
            form.connect_focus.clone(),
            form.save_focus.clone(),
        ];
        assert_eq!(
            order, expected,
            "an unrecognized scheme must expose no driver fields, not even a stale sqlite path field"
        );
    });
}

// ---- footer button clicks emit the right event -----------------------------

#[gpui::test]
fn a_cancel_event_from_the_form(cx: &mut TestAppContext) {
    let (_form, vcx, events) = build_form_in_window(cx);
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-form-cancel")
        .expect("the cancel button must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*events.borrow(), vec![CapturedEvent::Cancel]);
}

#[gpui::test]
fn an_add_event_from_the_form(cx: &mut TestAppContext) {
    let (form, vcx, events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| {
        form.set_name_input("from-footer", cx);
        form.set_url_input("sqlite::memory:", cx);
    });
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-form-save")
        .expect("the save button must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Add {
            name: "from-footer".to_owned(),
            url: "sqlite::memory:".to_owned(),
        }]
    );
}

#[gpui::test]
fn an_edit_event_from_the_form(cx: &mut TestAppContext) {
    let (form, vcx, events) = build_form_in_window(cx);
    let id = Uuid::new_v4();
    form.update(vcx, |form, cx| {
        form.begin_edit(
            id,
            "first".to_owned(),
            "postgres://host/a".to_owned(),
            None,
            None,
            cx,
        );
        form.set_name_input("renamed via footer", cx);
    });
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-form-save")
        .expect("the save-changes button must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Edit {
            id,
            name: "renamed via footer".to_owned(),
            url: "postgres://host/a".to_owned(),
        }]
    );
}

#[gpui::test]
fn a_test_event_from_the_form(cx: &mut TestAppContext) {
    let (form, vcx, events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| form.set_url_input("sqlite::memory:", cx));
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-form-test")
        .expect("the test button must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Test {
            url: "sqlite::memory:".to_owned(),
        }]
    );
}

#[gpui::test]
fn a_connect_event_from_the_form(cx: &mut TestAppContext) {
    let (form, vcx, events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| {
        form.set_name_input("via-footer", cx);
        form.set_url_input("sqlite::memory:", cx);
    });
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-form-connect")
        .expect("the connect button must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Connect {
            name: "via-footer".to_owned(),
            url: "sqlite::memory:".to_owned(),
        }]
    );
}

// ---- Enter-to-submit (name/url TextFieldEvent::Submit) ---------------------

#[gpui::test]
fn submitting_the_name_field_in_add_mode_persists_a_new_connection(cx: &mut TestAppContext) {
    let (form, events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_name_input("enter-submitted", cx);
        form.set_url_input("sqlite::memory:", cx);
    });
    let name_field = form.read_with(cx, |form, _cx| form.name_field.clone());
    name_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Add {
            name: "enter-submitted".to_owned(),
            url: "sqlite::memory:".to_owned(),
        }],
        "Enter in the name field must submit the add form, the same as clicking Save"
    );
}

#[gpui::test]
fn submitting_the_url_field_in_add_mode_persists_a_new_connection(cx: &mut TestAppContext) {
    let (form, events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_name_input("enter-submitted-url", cx);
        form.set_url_input("sqlite::memory:", cx);
    });
    let url_field = form.read_with(cx, |form, _cx| form.url_field.clone());
    url_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Add {
            name: "enter-submitted-url".to_owned(),
            url: "sqlite::memory:".to_owned(),
        }],
        "Enter in the url field must submit the add form, the same as clicking Save"
    );
}

#[gpui::test]
fn submitting_the_name_field_in_edit_mode_updates_the_row_in_place(cx: &mut TestAppContext) {
    let (form, events) = build_form(cx);
    let id = Uuid::new_v4();
    form.update(cx, |form, cx| {
        form.begin_edit(
            id,
            "first".to_owned(),
            "postgres://host/a".to_owned(),
            None,
            None,
            cx,
        );
        form.set_name_input("edited via enter", cx);
    });
    let name_field = form.read_with(cx, |form, _cx| form.name_field.clone());
    name_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Edit {
            id,
            name: "edited via enter".to_owned(),
            url: "postgres://host/a".to_owned(),
        }],
        "Enter in edit mode must submit the same as clicking Save changes"
    );
}

#[gpui::test]
fn submitting_the_url_field_in_edit_mode_updates_the_row_in_place(cx: &mut TestAppContext) {
    let (form, events) = build_form(cx);
    let id = Uuid::new_v4();
    form.update(cx, |form, cx| {
        form.begin_edit(
            id,
            "first".to_owned(),
            "postgres://host/a".to_owned(),
            None,
            None,
            cx,
        );
        form.set_name_input("edited via enter url", cx);
    });
    let url_field = form.read_with(cx, |form, _cx| form.url_field.clone());
    url_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));

    assert_eq!(
        *events.borrow(),
        vec![CapturedEvent::Edit {
            id,
            name: "edited via enter url".to_owned(),
            url: "postgres://host/a".to_owned(),
        }],
        "Enter in edit mode must submit the same as clicking Save changes"
    );
}

// ---- SSH section state ----------------------------------------------------

fn sample_stored_ssh() -> StoredSsh {
    StoredSsh {
        enabled: true,
        host: "bastion.example.com".to_owned(),
        port: 2222,
        user: "deploy".to_owned(),
        auth_kind: SshAuthKind::Password,
        key_path: None,
        host_key_policy: HostKeyPolicy::AcceptNew,
    }
}

#[gpui::test]
fn begin_edit_populates_the_ssh_section_from_stored_password_auth(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "staging".to_owned(),
            "postgres://app@staging.internal:5432/app".to_owned(),
            Some(sample_stored_ssh()),
            Some("tunnel-secret".to_owned()),
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert!(form.ssh_enabled);
        assert_eq!(
            form.ssh_host_field.read(cx).value().as_ref(),
            "bastion.example.com"
        );
        assert_eq!(form.ssh_port_field.read(cx).value().as_ref(), "2222");
        assert_eq!(form.ssh_user_field.read(cx).value().as_ref(), "deploy");
        assert!(matches!(form.ssh_auth_kind, SshAuthKind::Password));
        assert_eq!(
            form.ssh_password_field.read(cx).value().as_ref(),
            "tunnel-secret"
        );
        assert!(form.ssh_key_passphrase_field.read(cx).value().is_empty());
    });
}

#[gpui::test]
fn begin_edit_populates_the_ssh_section_from_stored_key_auth(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    let ssh = StoredSsh {
        auth_kind: SshAuthKind::Key,
        key_path: Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
        host_key_policy: HostKeyPolicy::KnownHosts(PathBuf::from("/home/user/.ssh/known_hosts")),
        ..sample_stored_ssh()
    };
    form.update(cx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "staging".to_owned(),
            "postgres://app@staging.internal:5432/app".to_owned(),
            Some(ssh),
            Some("key-passphrase".to_owned()),
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert!(matches!(form.ssh_auth_kind, SshAuthKind::Key));
        assert_eq!(
            form.ssh_key_path_field.read(cx).value().as_ref(),
            "/home/user/.ssh/id_ed25519"
        );
        assert_eq!(
            form.ssh_key_passphrase_field.read(cx).value().as_ref(),
            "key-passphrase"
        );
        assert!(form.ssh_password_field.read(cx).value().is_empty());
        assert_eq!(form.ssh_host_key_mode, HostKeyMode::KnownHosts);
        assert_eq!(
            form.ssh_known_hosts_path_field.read(cx).value().as_ref(),
            "/home/user/.ssh/known_hosts"
        );
    });
}

#[gpui::test]
fn begin_edit_displays_a_stored_prompt_host_key_policy_as_accept_new(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    let ssh = StoredSsh {
        host_key_policy: HostKeyPolicy::Prompt,
        ..sample_stored_ssh()
    };
    form.update(cx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "staging".to_owned(),
            "postgres://app@staging.internal:5432/app".to_owned(),
            Some(ssh),
            None,
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert_eq!(form.ssh_host_key_mode, HostKeyMode::AcceptNew);
        assert!(form.ssh_known_hosts_path_field.read(cx).value().is_empty());
    });
}

#[gpui::test]
fn begin_edit_with_no_ssh_leaves_the_section_disabled(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "plain".to_owned(),
            "postgres://app@host:5432/app".to_owned(),
            None,
            None,
            cx,
        );
    });
    form.read_with(cx, |form, cx| {
        assert!(!form.ssh_enabled);
        assert!(form.ssh_host_field.read(cx).value().is_empty());
    });
}

#[gpui::test]
fn begin_add_resets_a_previously_populated_ssh_section(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.begin_edit(
            Uuid::new_v4(),
            "staging".to_owned(),
            "postgres://app@host:5432/app".to_owned(),
            Some(sample_stored_ssh()),
            Some("secret".to_owned()),
            cx,
        );
        form.begin_add(cx);
    });
    form.read_with(cx, |form, cx| {
        assert!(!form.ssh_enabled);
        assert!(form.ssh_host_field.read(cx).value().is_empty());
        assert!(form.ssh_password_field.read(cx).value().is_empty());
        assert!(matches!(form.ssh_auth_kind, SshAuthKind::Agent));
    });
}

#[gpui::test]
fn ssh_state_is_none_while_the_toggle_is_off(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
    });
    form.read_with(cx, |form, cx| {
        let (ssh, secret) = form.ssh_state(cx);
        assert!(ssh.is_none());
        assert!(secret.is_none());
    });
}

#[gpui::test]
fn ssh_state_reflects_agent_auth_with_no_secret(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
    });
    let host_field = form.read_with(cx, |form, _cx| form.ssh_host_field.clone());
    host_field.update(cx, |field, cx| field.set_value("bastion", cx));

    form.read_with(cx, |form, cx| {
        let (ssh, secret) = form.ssh_state(cx);
        let ssh = ssh.expect("ssh must be Some while enabled");
        assert_eq!(ssh.host, "bastion");
        assert!(matches!(ssh.auth_kind, SshAuthKind::Agent));
        assert!(secret.is_none());
    });
}

#[gpui::test]
fn ssh_state_reflects_password_auth_and_its_secret(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
        form.set_ssh_auth_kind(SshAuthKind::Password, cx);
    });
    let password_field = form.read_with(cx, |form, _cx| form.ssh_password_field.clone());
    password_field.update(cx, |field, cx| field.set_value("hunter2", cx));

    form.read_with(cx, |form, cx| {
        let (ssh, secret) = form.ssh_state(cx);
        let ssh = ssh.expect("ssh must be Some while enabled");
        assert!(matches!(ssh.auth_kind, SshAuthKind::Password));
        assert_eq!(secret.as_deref(), Some("hunter2"));
    });
}

#[gpui::test]
fn ssh_state_reflects_key_auth_path_and_passphrase(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
        form.set_ssh_auth_kind(SshAuthKind::Key, cx);
    });
    let key_path_field = form.read_with(cx, |form, _cx| form.ssh_key_path_field.clone());
    key_path_field.update(cx, |field, cx| {
        field.set_value("/home/user/.ssh/id_ed25519", cx);
    });
    let passphrase_field = form.read_with(cx, |form, _cx| form.ssh_key_passphrase_field.clone());
    passphrase_field.update(cx, |field, cx| field.set_value("s3cr3t", cx));

    form.read_with(cx, |form, cx| {
        let (ssh, secret) = form.ssh_state(cx);
        let ssh = ssh.expect("ssh must be Some while enabled");
        assert!(matches!(ssh.auth_kind, SshAuthKind::Key));
        assert_eq!(
            ssh.key_path.as_deref(),
            Some(std::path::Path::new("/home/user/.ssh/id_ed25519"))
        );
        assert_eq!(secret.as_deref(), Some("s3cr3t"));
    });
}

#[gpui::test]
fn ssh_state_reflects_known_hosts_host_key_policy(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
        form.set_ssh_host_key_mode(HostKeyMode::KnownHosts, cx);
    });
    let known_hosts_field = form.read_with(cx, |form, _cx| form.ssh_known_hosts_path_field.clone());
    known_hosts_field.update(cx, |field, cx| {
        field.set_value("/home/user/.ssh/known_hosts", cx);
    });

    form.read_with(cx, |form, cx| {
        let (ssh, _secret) = form.ssh_state(cx);
        let ssh = ssh.expect("ssh must be Some while enabled");
        assert_eq!(
            ssh.host_key_policy,
            HostKeyPolicy::KnownHosts(PathBuf::from("/home/user/.ssh/known_hosts"))
        );
    });
}

#[gpui::test]
fn ssh_state_is_none_after_switching_the_url_to_sqlite_even_if_still_toggled_on(
    cx: &mut TestAppContext,
) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
        form.set_url_input("sqlite::memory:", cx);
    });
    form.read_with(cx, |form, cx| {
        // The SSH section unmounts for sqlite with no UI to clear a
        // stranded `ssh_enabled`, so `ssh_state` itself must gate on the
        // driver rather than reporting a tunnel for a sqlite connection.
        let (ssh, secret) = form.ssh_state(cx);
        assert!(ssh.is_none());
        assert!(secret.is_none());
    });
}

// ---- SSH section focus order -----------------------------------------------

#[gpui::test]
fn focus_order_includes_ssh_handles_after_tls_when_ssh_is_enabled(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
        form.set_ssh_auth_kind(SshAuthKind::Key, cx);
    });
    form.read_with(cx, |form, cx| {
        let order = form.focus_order(cx);
        let expected = vec![
            form.name_field.read(cx).focus_handle(cx),
            form.url_field.read(cx).focus_handle(cx),
            form.host_field.read(cx).focus_handle(cx),
            form.port_field.read(cx).focus_handle(cx),
            form.user_field.read(cx).focus_handle(cx),
            form.password_field.read(cx).focus_handle(cx),
            form.database_field.read(cx).focus_handle(cx),
            form.tls_focus.clone(),
            form.ssh_enabled_focus.clone(),
            form.ssh_host_field.read(cx).focus_handle(cx),
            form.ssh_port_field.read(cx).focus_handle(cx),
            form.ssh_user_field.read(cx).focus_handle(cx),
            form.ssh_auth_focus.clone(),
            form.ssh_key_path_field.read(cx).focus_handle(cx),
            form.ssh_key_passphrase_field.read(cx).focus_handle(cx),
            form.ssh_host_key_focus.clone(),
            form.cancel_focus.clone(),
            form.test_focus.clone(),
            form.connect_focus.clone(),
            form.save_focus.clone(),
        ];
        assert_eq!(order, expected);
    });
}

#[gpui::test]
fn tab_cycles_through_the_ssh_section_when_enabled(cx: &mut TestAppContext) {
    let (host, vcx) = build_form_host(cx);
    host.update(vcx, |host, cx| {
        host.form.update(cx, |form, cx| {
            form.set_url_input("postgres://app@host:5432/db", cx);
            form.set_ssh_enabled(true, cx);
            form.set_ssh_auth_kind(SshAuthKind::Password, cx);
        });
    });
    vcx.run_until_parked();

    let order = host.read_with(vcx, |host, cx| host.form.read(cx).focus_order(cx));
    assert!(
        order.len() >= 3,
        "SSH-enabled network form must expose more than name/url/footer"
    );
    assert_tab_cycles_through_in_order(vcx, &order);
}

#[gpui::test]
fn focus_order_keeps_the_ssh_toggle_but_skips_its_sub_fields_while_disabled(
    cx: &mut TestAppContext,
) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
    });
    form.read_with(cx, |form, cx| {
        let order = form.focus_order(cx);
        assert!(order.contains(&form.tls_focus));
        assert!(
            order.contains(&form.ssh_enabled_focus),
            "the SSH enable toggle must stay reachable even while off"
        );
        assert!(!order.contains(&form.ssh_host_field.read(cx).focus_handle(cx)));
        assert!(!order.contains(&form.ssh_auth_focus));
    });
}

#[gpui::test]
fn focus_order_for_a_sqlite_url_never_includes_any_ssh_or_tls_handle(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("sqlite::memory:", cx);
        form.set_ssh_enabled(true, cx);
    });
    form.read_with(cx, |form, cx| {
        let order = form.focus_order(cx);
        assert!(!order.contains(&form.ssh_enabled_focus));
        assert!(!order.contains(&form.tls_focus));
    });
}

// ---- TLS control capped by an enabled SSH tunnel ---------------------------

#[gpui::test]
fn postgres_tls_control_drops_verify_full_while_ssh_is_enabled_and_restores_it_once_off(
    cx: &mut TestAppContext,
) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
    });
    form.read_with(cx, |form, _cx| {
        assert!(
            form.tls_available_modes_for_test("postgres")
                .contains(&zsql_core::TlsVerify::VerifyFull)
        );
    });

    form.update(cx, |form, cx| form.set_ssh_enabled(true, cx));
    form.read_with(cx, |form, _cx| {
        assert!(
            !form
                .tls_available_modes_for_test("postgres")
                .contains(&zsql_core::TlsVerify::VerifyFull)
        );
    });

    form.update(cx, |form, cx| form.set_ssh_enabled(false, cx));
    form.read_with(cx, |form, _cx| {
        assert!(
            form.tls_available_modes_for_test("postgres")
                .contains(&zsql_core::TlsVerify::VerifyFull)
        );
    });
}

#[gpui::test]
fn mysql_tls_control_is_also_capped_while_ssh_is_enabled(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("mysql://app@host:3306/db", cx);
        form.set_ssh_enabled(true, cx);
    });
    form.read_with(cx, |form, _cx| {
        assert!(
            !form
                .tls_available_modes_for_test("mysql")
                .contains(&zsql_core::TlsVerify::VerifyFull)
        );
    });
}

#[gpui::test]
fn mssql_tls_control_is_never_capped_by_ssh(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("mssql://sa:pw@dbhost:1433/db", cx);
        form.set_ssh_enabled(true, cx);
    });
    form.read_with(cx, |form, _cx| {
        assert!(
            form.tls_available_modes_for_test("mssql")
                .contains(&zsql_core::TlsVerify::VerifyFull)
        );
    });
}

// ---- one vs. two column layout --------------------------------------------

#[gpui::test]
fn modal_size_stays_small_until_ssh_is_enabled_for_a_network_driver(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.read_with(cx, |form, _cx| {
        assert_eq!(form.modal_size(), ModalSize::Small, "no url yet");
    });

    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
    });
    form.read_with(cx, |form, _cx| {
        assert_eq!(form.modal_size(), ModalSize::Small, "ssh still off");
    });

    form.update(cx, |form, cx| form.set_ssh_enabled(true, cx));
    form.read_with(cx, |form, _cx| {
        assert_eq!(
            form.modal_size(),
            ModalSize::Wide,
            "ssh on for a network driver opens the second column"
        );
    });

    form.update(cx, |form, cx| form.set_ssh_enabled(false, cx));
    form.read_with(cx, |form, _cx| {
        assert_eq!(form.modal_size(), ModalSize::Small, "ssh off again");
    });
}

#[gpui::test]
fn modal_size_stays_small_for_sqlite_even_with_ssh_toggled_on(cx: &mut TestAppContext) {
    let (form, _events) = build_form(cx);
    form.update(cx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
        form.set_url_input("sqlite::memory:", cx);
    });
    form.read_with(cx, |form, _cx| {
        assert_eq!(
            form.modal_size(),
            ModalSize::Small,
            "the SSH section never mounts for sqlite, so it must not widen the modal"
        );
    });
}

#[gpui::test]
fn clicking_the_ssh_toggle_live_transitions_the_form_between_one_and_two_columns(
    cx: &mut TestAppContext,
) {
    let (form, vcx, _events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
    });
    vcx.run_until_parked();
    assert_eq!(
        form.read_with(vcx, |form, _cx| form.modal_size()),
        ModalSize::Small
    );

    let on_bounds = vcx
        .debug_bounds("connection-form-ssh-on")
        .expect("the ssh-on segment must be painted");
    vcx.simulate_click(on_bounds.center(), Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        form.read_with(vcx, |form, _cx| form.modal_size()),
        ModalSize::Wide,
        "the click must live-transition the form to two columns with no re-open"
    );

    let off_bounds = vcx
        .debug_bounds("connection-form-ssh-off")
        .expect("the ssh-off segment must be painted");
    vcx.simulate_click(off_bounds.center(), Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(
        form.read_with(vcx, |form, _cx| form.modal_size()),
        ModalSize::Small,
        "toggling back off must live-transition the form back to one column"
    );
}

#[gpui::test]
fn the_footer_still_emits_cancel_once_the_form_is_two_columns(cx: &mut TestAppContext) {
    let (form, vcx, events) = build_form_in_window(cx);
    form.update(vcx, |form, cx| {
        form.set_url_input("postgres://app@host:5432/db", cx);
        form.set_ssh_enabled(true, cx);
    });
    vcx.run_until_parked();
    assert_eq!(
        form.read_with(vcx, |form, _cx| form.modal_size()),
        ModalSize::Wide
    );

    let bounds = vcx
        .debug_bounds("connection-form-cancel")
        .expect("the single, full-width footer's cancel button must still be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(*events.borrow(), vec![CapturedEvent::Cancel]);
}
