//! The SQL editor: a framework-independent text-editing model (`TextBuffer`,
//! cursor/selection, a syntax-highlighting seam) plus the `gpui` view built
//! on top of it (`EditorView`). Running the current query is injected by the
//! embedding app through the [`QueryRunner`] seam `EditorView::new` takes, so
//! this crate never names an app, driver, or session type -- it depends only
//! on `gpui` and `zsql-ui` (for the shared color palette).

mod bindings;
mod buffer;
mod highlighter;
mod theme;
mod view;

pub use bindings::EditorBindings;
pub use buffer::{Position, Selection, TextBuffer};
pub use highlighter::{HighlightKind, Highlighter, PlainHighlighter, SqlHighlighter, StyleSpan};
pub use theme::{EDITOR_LINE_HEIGHT, syntax_color};
pub use view::{
    EditListener, EditorView, KEY_CONTEXT, QueryRunner, RunQuery, SaveRequester, SaveScript,
    SaveScriptAs, init,
};
