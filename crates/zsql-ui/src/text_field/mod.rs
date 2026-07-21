//! A reusable, interactive single-line text input: a framework-independent
//! editing model (`FieldModel`, byte-offset cursor/selection, UTF-16
//! conversions, a cursor-blink state machine) plus the `gpui` state entity
//! and element built on top of it (`TextFieldState`). Depends only on
//! `gpui` and this crate's own color palette, so it knows nothing about any
//! app, driver, or session type.

mod model;
// Crate-visible (not just module-private) so the shared theme's metric
// defaults can assert they still match this field's own established radius.
pub(crate) mod theme;
mod view;

pub use model::{BlinkState, CURSOR_BLINK_INTERVAL, CURSOR_BLINK_RESUME_DELAY, FieldModel};
pub use view::{KEY_CONTEXT, TextFieldEvent, TextFieldState, init};
