//! zsql — a lightweight Postgres-first SQL editor (gpui).
//!
//! The window opens straight into the results grid (`ui::results::ResultsView`)
//! populated with a hardcoded sample result set. There is no live connection or
//! editor pane wired up yet — this view exists to get the grid's rendering,
//! layout, and cell formatting right in isolation before a real `Session` feeds
//! it query results.

mod config;
mod observability;
mod ui;

use config::Config;
use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use ui::results::ResultsView;
use zsql_core::{ColumnMeta, ResultSet, Row, Value};

/// Default window size for the results-grid preview.
const WINDOW_WIDTH: f32 = 1180.0;
/// Default window size for the results-grid preview.
const WINDOW_HEIGHT: f32 = 760.0;

/// A hardcoded `orders` result set matching the locked visual spec, used to
/// populate the grid until a real `Session` runs live queries against it.
fn sample_result_set() -> ResultSet {
    let columns = vec![
        ColumnMeta {
            name: "id".to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        },
        ColumnMeta {
            name: "user_id".to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        },
        ColumnMeta {
            name: "total_cents".to_owned(),
            type_name: "int4".to_owned(),
            nullable: false,
        },
        ColumnMeta {
            name: "status".to_owned(),
            type_name: "text".to_owned(),
            nullable: false,
        },
        ColumnMeta {
            name: "metadata".to_owned(),
            type_name: "jsonb".to_owned(),
            nullable: true,
        },
        ColumnMeta {
            name: "placed_at".to_owned(),
            type_name: "timestamptz".to_owned(),
            nullable: true,
        },
    ];

    let rows = vec![
        Row(vec![
            Value::Int(1),
            Value::Int(1),
            Value::Int(1299),
            Value::Text("paid".to_owned()),
            Value::Json(r#"{"coupon": "WELCOME"}"#.to_owned()),
            Value::Timestamp("2026-07-14T09:12:31+00:00".to_owned()),
        ]),
        Row(vec![
            Value::Int(2),
            Value::Int(1),
            Value::Int(4900),
            Value::Text("pending".to_owned()),
            Value::Json("{}".to_owned()),
            Value::Timestamp("2026-07-15T18:03:02+00:00".to_owned()),
        ]),
        Row(vec![
            Value::Int(3),
            Value::Int(2),
            Value::Int(250),
            Value::Text("refunded".to_owned()),
            Value::Json(r#"{"reason": "duplicate"}"#.to_owned()),
            Value::Null,
        ]),
    ];

    ResultSet {
        columns,
        rows,
        affected: None,
        notices: Vec::new(),
    }
}

fn main() -> anyhow::Result<()> {
    observability::init();

    let cfg = match Config::default_path() {
        Some(path) => Config::load_or_default(&path)?,
        None => Config::default(),
    };
    // No connection is opened in this binary yet (the grid below is fed a
    // hardcoded result set), but resolving the configured URL here exercises
    // Config's wiring end-to-end ahead of a real session using it to connect.
    let has_configured_url = cfg.resolve_url().is_some();
    tracing::info!(theme = %cfg.theme.name, has_configured_url, "zsql starting");

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|_cx| ResultsView::new(sample_result_set(), "public.orders")),
        )
        .expect("failed to open window");
        cx.activate(true);
    });

    Ok(())
}
