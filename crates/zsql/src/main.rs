//! zsql — a lightweight Postgres-first SQL editor (gpui).
//!
//! M0: a single window whose body shows the result of the sqlx-on-smol spike
//! (`SELECT 1` awaited on gpui's executor, no tokio). This proves the async seam
//! end-to-end before the real editor/sidebar/grid land in later milestones.

mod config;
mod observability;

use config::Config;
use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};

/// Root view for M0: displays the spike status.
struct SpikeView {
    status: SharedString,
}

impl SpikeView {
    /// Build the view. If a connection URL is available, kick off the spike on
    /// gpui's executor and update `status` when it resolves.
    fn new(url: Option<String>, cx: &mut Context<Self>) -> Self {
        let Some(url) = url else {
            return Self {
                status: "set DATABASE_URL to run the SELECT 1 spike".into(),
            };
        };

        // Await the sqlx future directly on gpui's async context — this is the
        // seam M0 exists to validate. (The production query path will hop to the
        // background executor; SELECT 1 is trivial enough to run inline here.)
        cx.spawn(async move |this, cx| {
            let msg = match zsql_postgres::spike_select_one(&url).await {
                Ok(n) => {
                    tracing::info!(result = n, "spike ok");
                    format!("SELECT 1 -> {n}   (sqlx runtime-smol on gpui executor: OK)")
                }
                Err(e) => {
                    tracing::error!(error = %e, "spike failed");
                    format!("spike failed: {e}")
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.status = msg.into();
                cx.notify();
            });
        })
        .detach();

        Self {
            status: "connecting…".into(),
        }
    }
}

impl Render for SpikeView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .bg(rgb(0x1e_1e_2e))
            .justify_center()
            .items_center()
            .text_color(rgb(0xcd_d6_f4))
            .child(div().text_xl().child("zsql — M0 spike"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x93_99_b2))
                    .child(self.status.clone()),
            )
    }
}

fn main() -> anyhow::Result<()> {
    observability::init();

    let cfg = match Config::default_path() {
        Some(path) => Config::load_or_default(&path)?,
        None => Config::default(),
    };
    let url = cfg.resolve_url();
    tracing::info!(theme = %cfg.theme.name, has_url = url.is_some(), "zsql M0 starting");

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| SpikeView::new(url.clone(), cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });

    Ok(())
}
